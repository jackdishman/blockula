//! What the engine's configuration (4.2) accepts and how it meters, through the engine-neutral
//! boundary only (soroban-wasmi, and wasmi 0.38 with `--features ref038`). Executable notes for
//! `validate.rs`: where the engine accepts what 4.2 refuses, validation has to refuse it.

use blockula_vm::engine::{BlockEngine, EngineKind, Host, Instance, Invoked, Trap, new_block_engine};
use blockula_vm::meter::V1;

struct NoHost;
impl Host for NoHost {
    fn call(&mut self, _: usize, _: &[i64], _: &mut dyn Instance) -> Result<i64, Trap> {
        Ok(0)
    }
}

fn engine(kind: EngineKind, body: &str) -> (Box<dyn BlockEngine>, Result<(), String>) {
    let wasm = wat::parse_str(format!(r#"(module (memory (export "memory") 1 1) {body})"#)).unwrap();
    let mut e = new_block_engine(kind, &V1);
    let loaded = e.load([0; 32], &wasm).map_err(|e| e.to_string());
    (e, loaded)
}

fn loads(kind: EngineKind, body: &str) -> bool {
    engine(kind, body).1.is_ok()
}

fn invoke(e: &dyn BlockEngine, fuel: u64) -> Invoked {
    e.invoke(&[0; 32], "bk_x", fuel, &mut NoHost)
}

/// `floats(false)` refuses float types and operators on every engine, except float→int
/// conversions and reinterprets in unreachable code (the operand stack is polymorphic there and
/// these operators never check the float feature). They can't execute, but 4.3 says no float
/// instruction validates, so `validate.rs` must refuse them itself.
#[test]
fn floats_off_leaves_conversions_in_unreachable_code() {
    for kind in EngineKind::ALL {
        for op in ["i32.trunc_f32_s", "i64.trunc_f64_u", "i32.reinterpret_f32", "i64.reinterpret_f64"] {
            assert!(loads(*kind, &format!(r#"(func (export "bk_x") unreachable (drop ({op})))"#)), "{}: {op}", kind.name());
        }
        for body in ["unreachable (drop (f32.add))", "(drop (f32.const 1))", "(drop (f64.load (i32.const 0)))"] {
            assert!(!loads(*kind, &format!(r#"(func (export "bk_x") {body})"#)), "{}: {body}", kind.name());
        }
        assert!(!loads(*kind, r#"(func (export "bk_x") (param f32))"#), "{}: a float type", kind.name());
    }
}

/// The off proposals are refused at load on every engine.
#[test]
fn off_proposals_are_refused() {
    for kind in EngineKind::ALL {
        for body in [
            "(drop (i32.extend8_s (i32.const 1)))",                     // sign-extension
            "(memory.fill (i32.const 0) (i32.const 0) (i32.const 1))",  // bulk memory
            "unreachable (drop (i32.trunc_sat_f32_s))",                 // saturating float→int
        ] {
            assert!(!loads(*kind, &format!(r#"(func (export "bk_x") {body})"#)), "{}: {body}", kind.name());
        }
        assert!(!loads(*kind, r#"(func (export "bk_x") (result i32 i32) (i32.const 1) (i32.const 2))"#), "{}: multi-value", kind.name());
    }
}

/// What rustc's `wasm32v1-none` output needs loads everywhere: a funcref table with
/// `call_indirect` (dyn calls) and an internal mutable global (the stack pointer).
#[test]
fn what_rust_emits_loads() {
    for kind in EngineKind::ALL {
        let table = r#"(type $t (func (result i32))) (table 2 2 funcref) (elem (i32.const 0) $a $b)
            (func $a (result i32) (i32.const 1)) (func $b (result i32) (i32.const 2))
            (func (export "bk_x") (drop (call_indirect (type $t) (i32.const 1))))"#;
        let (e, loaded) = engine(*kind, table);
        assert_eq!(loaded, Ok(()), "{}", kind.name());
        assert!(invoke(e.as_ref(), 1_000).result.is_ok(), "{}", kind.name());
        assert!(loads(*kind, r#"(global $sp (mut i32) (i32.const 65536)) (func (export "bk_x") (global.set $sp (i32.const 1)))"#));
    }
}

/// A start section: the engines load it and refuse at instantiation (`ensure_no_start`), as a node
/// fault, so it never runs; `validate.rs` refuses it at upload so no stored code has one.
#[test]
fn a_start_function_never_runs() {
    for kind in EngineKind::ALL {
        let (e, loaded) = engine(*kind, r#"(func $s unreachable) (start $s) (func (export "bk_x"))"#);
        assert_eq!(loaded, Ok(()), "{}", kind.name());
        assert!(matches!(invoke(e.as_ref(), 1_000).result, Err(Trap::Fault(_))), "{}", kind.name());
    }
}

/// Out of fuel, the engine leaves the remainder from before the charge that failed (the whole
/// block is charged at its entry), and that remainder depends on the engine's cost model: why the
/// executor counts a frame that runs out as having used all its share, and the receipt says
/// `fuel_limit`.
#[test]
fn out_of_fuel_leaves_a_remainder() {
    for kind in EngineKind::ALL {
        let (e, _) = engine(*kind, r#"(func (export "bk_x") (loop $l (br $l)))"#);
        let r = [invoke(e.as_ref(), 1_000), invoke(e.as_ref(), 1_001)];
        assert!(r.iter().all(|r| r.result == Err(Trap::OutOfFuel)));
        assert!(r.iter().any(|r| r.fuel_left > 0), "{}", kind.name());
    }
}

/// What the engines do with a `memory.grow`, which upload validation never lets through
/// (`validate::Rule::MemoryGrow`), loaded here on each engine directly. The memory is fixed
/// (initial = maximum), so a grow returns −1, or the size for a grow by 0. What a refused grow
/// costs is the engine's own: soroban-wasmi charges 1,024 fuel per requested page before refusing
/// (Eager; Lazy checks the same amount first), wasmi 0.38 refuses at its static cost. A grow can
/// do nothing a program needs, so validation refuses it rather than leave its cost to the engine.
#[test]
fn what_the_engines_do_with_a_grow_validation_never_lets_through() {
    // Traps (`unreachable`) unless `memory.grow` by `pages` returns `expect`.
    let grow = |pages: i32, expect: i32| {
        format!(r#"(func (export "bk_x") (if (i32.ne (memory.grow (i32.const {pages})) (i32.const {expect})) (then unreachable)))"#)
    };
    const FUEL: u64 = 10_000_000;
    let mut left = Vec::new();
    for kind in EngineKind::ALL {
        let name = kind.name();
        for (pages, expect) in [(0, 1), (1, -1), (1_000, -1)] {
            let (e, loaded) = engine(*kind, &grow(pages, expect));
            assert_eq!(loaded, Ok(()), "{name}: an engine loads memory.grow; only validation refuses it");
            let r = invoke(e.as_ref(), FUEL);
            assert_eq!(r.result, Ok(()), "{name}: memory.grow {pages}");
            if pages == 1_000 {
                left.push((*kind, r.fuel_left));
            }
        }
        // 1,000 pages with 200,000 fuel: soroban-wasmi's 1,024,000 for the pages isn't there.
        let (e, _) = engine(*kind, &grow(1_000, -1));
        let r = invoke(e.as_ref(), 200_000);
        match kind {
            EngineKind::Soroban => assert_eq!(r.result, Err(Trap::OutOfFuel), "{name}"),
            #[cfg(feature = "ref038")]
            EngineKind::Wasmi038 => assert_eq!(r.result, Ok(()), "{name}: −1 at the static cost"),
        }
    }
    for (kind, left) in left {
        let used = FUEL - left;
        match kind {
            EngineKind::Soroban => assert!(used >= 1_024_000, "{}: {used}", kind.name()),
            #[cfg(feature = "ref038")]
            EngineKind::Wasmi038 => assert!(used < 1_000, "{}: {used}", kind.name()),
        }
    }
}
