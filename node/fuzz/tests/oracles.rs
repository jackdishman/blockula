//! The v2 fixture's oracles see what a bug in the node would change (docs/PROGRAMS-V0.md §10.4).
//!
//! Each test runs a seed with a change made to the state or the receipt right after the unrolled
//! block applies a transaction (`harness::state_machine_programs_tampered`) — the change a defect
//! in the node's code would make — and the run must stop at the oracle that holds that fact to the
//! spec or to the state's own deltas. A clean run of the same seed is the other half
//! (`tests/harness.rs`), so each oracle is shown to fail when it should and pass when it should.
//! The campaign-level proof is the same on the node's own code: the report's mutation table.

use blockula_fuzz::chain::State;
use blockula_fuzz::harness;
use blockula_fuzz::programs::receipt::ProgramReceipt;
use blockula_fuzz::programs::state::{Fault, FaultClass};
use blockula_fuzz::types::{Action, Tx};
use std::sync::atomic::{AtomicBool, Ordering};

fn seed(name: &str) -> Vec<u8> {
    harness::state_machine_programs_seeds().into_iter().find(|(n, _)| *n == name).unwrap_or_else(|| panic!("no seed {name}")).1
}

/// The message of the panic `f` raises (none if it returns).
fn panic_of<R>(f: impl FnOnce() -> R + std::panic::UnwindSafe) -> Option<String> {
    std::panic::catch_unwind(f).err().map(|e| e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default())
}

/// Runs `seed_name` tampered by `tamper` and requires the panic to say `expected`.
fn sees(seed_name: &str, tamper: fn(&mut State, &Tx), expected: &str) {
    let data = seed(seed_name);
    let msg = panic_of(|| harness::state_machine_programs_tampered(&data, tamper)).unwrap_or_else(|| panic!("{seed_name}: the tampered run passed every oracle (wanted: {expected})"));
    assert!(msg.contains(expected), "{seed_name}: the oracle that fired says\n  {msg}\nwanted a message with `{expected}`");
}

/// The newest receipt of the block run: the one the transaction that just applied left.
fn newest(state: &mut State) -> &mut ProgramReceipt {
    &mut state.programs.block.receipts.last_mut().expect("a program transaction left a receipt").1
}

/// Defines a tamper that acts once, on the first transaction `$when` says yes to.
macro_rules! tamper {
    ($name:ident, |$state:ident, $tx:ident| when $when:expr => $act:expr) => {
        fn $name($state: &mut State, $tx: &Tx) {
            static DONE: AtomicBool = AtomicBool::new(false);
            if $when && !DONE.swap(true, Ordering::SeqCst) {
                $act
            }
        }
    };
}

fn is_call(tx: &Tx, method: &str) -> bool {
    matches!(&tx.action, Action::CallProgram { calls, .. } if calls.iter().any(|c| c.method == method))
}

// ---- the fee and the fuel (S8F-R2)

tamper!(fuel_fee_a_drop_high, |state, tx| when matches!(tx.action, Action::CallProgram { .. }) => newest(state).fee.fuel += 1);

/// A fuel fee rounded the wrong way, or any other part of the charge a drop out, is not the spec's.
#[test]
fn a_fuel_fee_a_drop_off_is_seen() {
    sees("programs-counter-and-token", fuel_fee_a_drop_high, "charged other parts than the spec's");
}

tamper!(fuel_counted_one_high, |state, tx| when matches!(tx.action, Action::CallProgram { .. }) => state.programs.block.fuel += 1);

/// A block that counts a unit of fuel more than its transactions reserved.
#[test]
fn a_block_that_counts_a_fuel_too_many_is_seen() {
    sees("programs-counter-and-token", fuel_counted_one_high, "counted");
}

tamper!(fuel_rate_one_thousandth_high, |state, tx| when matches!(tx.action, Action::CallProgram { .. }) => state.programs.meta.as_mut().expect("META").fuel_rate += 1_000);

/// A fuel rate that doesn't follow §5.6's controller from the block's counted fuel.
#[test]
fn a_fuel_rate_off_the_controller_is_seen() {
    sees("programs-counter-and-token", fuel_rate_one_thousandth_high, "by §5.6");
}

tamper!(reserved_a_unit_high, |state, tx| when matches!(tx.action, Action::CallProgram { .. }) => newest(state).fuel.reserved += 1);

/// A receipt whose reservation isn't `exec + load`.
#[test]
fn a_reservation_off_by_one_is_seen() {
    sees("programs-counter-and-token", reserved_a_unit_high, "the spec says exec");
}

// ---- the receipt's facts against the state (S8F-R3)

tamper!(created_names_another, |state, tx| when matches!(tx.action, Action::CreateProgram { .. }) => newest(state).created = Some("boo1someotherprogram".into()));

#[test]
fn a_receipt_that_names_another_program_is_seen() {
    sees("programs-create-pay-upload", created_names_another, "a create's receipt names");
}

tamper!(upload_used_one_low, |state, tx| when matches!(tx.action, Action::UploadCode { .. }) && newest_is_ok(state) => newest(state).fuel.used -= 1);

fn newest_is_ok(state: &State) -> bool {
    state.programs.block.receipts.last().is_some_and(|(_, r)| r.outcome.is_ok())
}

#[test]
fn an_uploads_fuel_used_one_low_is_seen() {
    sees("programs-create-pay-upload", upload_used_one_low, "all of it");
}

tamper!(upload_deposit_a_drop_high, |state, tx| when matches!(tx.action, Action::UploadCode { .. }) && newest_is_ok(state) => newest(state).upload.as_mut().expect("an upload").deposit += 1);

#[test]
fn an_uploads_deposit_a_drop_high_is_seen() {
    sees("programs-create-pay-upload", upload_deposit_a_drop_high, "an upload's receipt says deposit");
}

tamper!(bytes_delta_one_high, |state, tx| when matches!(tx.action, Action::UploadCode { .. }) && newest_is_ok(state) => newest(state).storage.bytes_delta += 1);

#[test]
fn a_storage_delta_one_byte_high_is_seen() {
    sees("programs-create-pay-upload", bytes_delta_one_high, "program state grew");
}

tamper!(bonded_a_drop_high, |state, tx| when is_call(tx, "open") && newest_is_ok(state) => newest(state).storage.bonded += 1);

#[test]
fn a_bonded_total_a_drop_high_is_seen() {
    sees("programs-escrow", bonded_a_drop_high, "it bonded");
}

tamper!(creation_fee_a_drop_high, |state, tx| when matches!(tx.action, Action::CreateProgram { .. }) && newest_is_ok(state) => newest(state).outside_vm.account_creation_fee += 1);

/// What a create's receipt says it debited outside the VM must be what its signer lost.
#[test]
fn a_debit_outside_the_vm_a_drop_high_is_seen() {
    sees("programs-create-pay-upload", creation_fee_a_drop_high, "the signer of a create_program");
}

// ---- where the money went (S8F-R1)

tamper!(payout_a_drop_high_to_the_seller, |state, tx| when is_call(tx, "release") && newest_is_ok(state) => {
    let seller = newest(state).balance_changes.iter().find(|b| b.change > 0).map(|b| b.account.clone()).expect("a release pays someone");
    state.accounts.get_mut(&seller).expect("the seller has an account").balance += 1;
});

/// FANG that lands on the wrong account (or a drop too much on the right one) while the signer's
/// own balance reconciles.
#[test]
fn a_payout_that_lands_wrong_is_seen() {
    sees("programs-escrow", payout_a_drop_high_to_the_seller, "went from");
}

// ---- the guests' storage (S8F-R13)

tamper!(counter_stale, |state, tx| when is_call(tx, "increment") && newest_is_ok(state) => {
    let key = state.programs.storage.iter().find(|(k, e)| k.0.ends_with(b"n") && e.value.len() == 8).map(|(k, _)| k.clone()).expect("the counter stored its count");
    let e = state.programs.storage.get_mut(&key).expect("an entry");
    e.value[0] = e.value[0].wrapping_sub(1);
});

/// A write the commit dropped or left stale: the counter's count isn't what the increments that
/// applied make it.
#[test]
fn a_stale_stored_value_is_seen() {
    sees("programs-counter-and-token", counter_stale, "the counter");
}

tamper!(token_balance_a_unit_high, |state, tx| when is_call(tx, "transfer") && newest_is_ok(state) => {
    let key = state.programs.storage.iter().find(|(k, e)| k.0.len() == 32 + 33 && k.0[32] == b'b' && e.value.len() == 8).map(|(k, _)| k.clone()).expect("a balance");
    let e = state.programs.storage.get_mut(&key).expect("an entry");
    e.value[0] = e.value[0].wrapping_add(1);
});

#[test]
fn a_token_balance_that_breaks_the_supply_is_seen() {
    sees("programs-counter-and-token", token_balance_a_unit_high, "balances after block");
}

// ---- faults (S8F-R8)

fn fault(state: &mut State, class: FaultClass) {
    state.programs.fault = Some(Fault { height: 3, tx: "00".repeat(32), class, what: "planted by a test".into() });
}

tamper!(a_tx_fault, |state, tx| when matches!(tx.action, Action::CallProgram { .. }) => fault(state, FaultClass::Tx));

/// A Tx-class fault is a finding: no valid transaction can cause one.
#[test]
fn a_tx_class_fault_is_a_finding() {
    sees("programs-counter-and-token", a_tx_fault, "a program transaction faulted the node");
}

fn setup_fault_always(state: &mut State, tx: &Tx) {
    if matches!(tx.action, Action::CallProgram { .. }) {
        fault(state, FaultClass::Setup);
    }
}

/// A Setup-class fault is the machine's, not the code's, and not a finding: the block is run once
/// more, and if the machine can't run it the input ends there, unjudged.
#[test]
fn a_setup_class_fault_that_persists_is_a_resource_error_not_a_finding() {
    let data = seed("programs-counter-and-token");
    assert_eq!(harness::state_machine_programs_tampered(&data, setup_fault_always), None);
    assert_eq!(harness::state_machine_programs_tampered(&data, |_, _| {}), Some(true), "the same input untampered runs");
}

fn setup_fault_once(state: &mut State, tx: &Tx) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if matches!(tx.action, Action::CallProgram { .. }) && !DONE.swap(true, Ordering::SeqCst) {
        fault(state, FaultClass::Setup);
    }
}

/// A Setup-class fault that doesn't come back on the retry leaves the input run in full.
#[test]
fn a_setup_class_fault_that_does_not_return_is_retried_away() {
    let data = seed("programs-counter-and-token");
    assert_eq!(harness::state_machine_programs_tampered(&data, setup_fault_once), Some(true));
}

// ---- program_commit: what `apply_diff` wrote (S8F-R1, R13)

use blockula_vm::exec::Diff;

/// Runs `program_commit`'s seed `name` with `tamper` applied to the state after `apply_diff`, and
/// requires the panic to say `expected`.
fn commit_sees(name: &str, tamper: fn(&mut State, &Diff), expected: &str) {
    let data = harness::program_commit_seeds().into_iter().find(|(n, _)| *n == name).unwrap_or_else(|| panic!("no seed {name}")).1;
    let msg = panic_of(|| harness::commit_tampered(&data, Some(tamper))).unwrap_or_else(|| panic!("{name}: the tampered commit passed every check (wanted: {expected})"));
    assert!(msg.contains(expected), "{name}: the check that fired says\n  {msg}\nwanted a message with `{expected}`");
}

/// The first account the diff credits, other than the signer's.
fn credited(diff: &Diff, state: &State) -> String {
    let (account, _) = diff.balances.iter().find(|(_, d)| *d > 0).expect("the diff pays someone");
    blockula_fuzz::crypto::render_address(state.params.prefix(), account)
}

fn pay_a_drop_too_much(state: &mut State, diff: &Diff) {
    let who = credited(diff, state);
    state.accounts.get_mut(&who).expect("the payee").balance += 1;
}

fn pay_a_drop_to_the_wrong_account(state: &mut State, diff: &Diff) {
    let who = credited(diff, state);
    state.accounts.get_mut(&who).expect("the payee").balance -= 1;
    let other = state.accounts.keys().find(|a| **a != who).cloned().expect("another account");
    state.accounts.get_mut(&other).expect("an account").balance += 1;
}

#[test]
fn a_payout_a_drop_too_much_or_to_the_wrong_account_is_seen_by_program_commit() {
    commit_sees("escrow-release-to-a-user", pay_a_drop_too_much, "went from");
    commit_sees("escrow-release-to-a-user", pay_a_drop_to_the_wrong_account, "went from");
}

fn stale_value(state: &mut State, diff: &Diff) {
    let c = diff.storage.iter().find(|c| c.entry.is_some()).expect("the diff writes");
    let key = blockula_fuzz::programs::state::storage_key(&c.program, &c.key);
    let e = state.programs.storage.get_mut(&key).expect("the entry");
    e.value[0] ^= 1;
}

fn dropped_write(state: &mut State, diff: &Diff) {
    let c = diff.storage.iter().find(|c| c.entry.is_some()).expect("the diff writes");
    let key = blockula_fuzz::programs::state::storage_key(&c.program, &c.key);
    state.programs.storage.remove(&key);
}

#[test]
fn a_stored_value_that_is_stale_or_dropped_is_seen_by_program_commit() {
    commit_sees("counter-first-increment", stale_value, "reads");
    commit_sees("counter-first-increment", dropped_write, "reads");
}

fn escrow_off_by_a_drop(state: &mut State, _: &Diff) {
    state.programs.meta.as_mut().expect("META").escrow += 1;
}

#[test]
fn programs_escrow_a_drop_off_is_seen_by_program_commit() {
    commit_sees("counter-first-increment", escrow_off_by_a_drop, "escrow");
}
