//! Fuzz harnesses for the programs engine (S4). The `fuzz_targets/*` binaries are thin
//! `fuzz_target!` wrappers over these functions, so the logic can be exercised on stable by the
//! `#[cfg(test)]` seed checks below.
//!
//! The invariants, from docs/PROGRAMS.md 4.2–4.4:
//! * [`program_validate`]: `validate()` never panics, and everything it accepts uploads: the test
//!   translation neither panics nor refuses it.
//! * [`program_exec`]: the executor, host and overlay never panic, corrupt memory or report a node
//!   fault (`MemHost` is a sound view, so one would be the executor's bug), however a batch of calls
//!   to the hand-written guests is shaped: callees, methods, inputs, values, the kinds and
//!   payability their interfaces declare, the kind each call runs as, and stale version digests.
//!   Only validated code runs, as on a node.

use arbitrary::Arbitrary;
use blockula_vm::engine::EngineKind;
use blockula_vm::exec::{ANY_VERSION, Call, TxLimits, execute};
use blockula_vm::host::MethodKind;
use blockula_vm::meter::SCHEDULE_V0;
use blockula_vm::testing::{MemHost, addr};
use blockula_vm::upload::Refusal;

/// `validate()` must never panic, and every module it accepts must upload. `upload()` runs the
/// test translation under `catch_unwind` and reports a panic as a node fault, which we turn back
/// into a panic for the fuzzer to catch; a refusal is a disagreement between validation and the
/// engine, also a finding.
pub fn program_validate(data: &[u8]) {
    if blockula_vm::validate::validate(data).is_err() {
        return;
    }
    let mut host = MemHost::new();
    match host.upload(data) {
        Ok(Ok(_)) => {}
        Ok(Err(Refusal::Invalid(e))) => panic!("validate() accepted a module upload() then refused ({:?})", e.rule),
        Ok(Err(Refusal::Translate(_))) => panic!("validate() accepted a module the engine refused"),
        Err(fault) => panic!("validate() accepted a module whose upload is a node fault: {fault}"),
    }
}

/// The four hand-written guests (S2), deployed for every run so the executor has real code to run.
const GUESTS: &[(&str, &[u8])] = &[
    ("counter", include_bytes!("../../tests/programs/counter.wasm")),
    ("token", include_bytes!("../../tests/programs/token.wasm")),
    ("escrow", include_bytes!("../../tests/programs/escrow.wasm")),
    ("router", include_bytes!("../../tests/programs/router.wasm")),
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
    }

    #[test]
    fn program_exec_accepts_its_seeds() {
        for seed in seeds("program_exec") {
            if let Ok(scenario) = Scenario::arbitrary_take_rest(Unstructured::new(&seed)) {
                program_exec(scenario);
            }
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
