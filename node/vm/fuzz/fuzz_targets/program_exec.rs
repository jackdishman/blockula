#![no_main]
//! See `blockula_vm_fuzz::program_exec`: the executor, host and overlay never panic or corrupt
//! memory, however a batch of calls to the guests is shaped.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|scenario: blockula_vm_fuzz::Scenario| {
    blockula_vm_fuzz::program_exec(scenario);
});
