//! Programs' admission rules (docs/PROGRAMS-V0.md §1.1, §2, §5.4): the last check of `State::admit`,
//! after Ghost Mode's, so they hold on every path a transaction takes (mempool admission and
//! revalidation, a proposer's trial, block verification, execution and replay). A refusal is
//! `Err`: the transaction can't be included, and nobody is charged.
//!
//! **For every transaction:** none acts from a program's account (a program has no keys, §2.1),
//! and a `ParamChange` of one of programs' parameters (`program_*`, §4.5), which the codec lets
//! through so that a block can carry one once programs exist, is refused in the words v1's codec
//! refuses an unknown key with until then ([`super::params::admit`]).
//!
//! **For a program action** (tags 62–64 and 69), first the [`status`] at the height:
//!
//! | Status | Upload (62) | Create (63) | Call (64) | Pause (69) |
//! |---|---|---|---|---|
//! | `Dormant` (below v2, or programs never switched on) | refused | refused | refused | refused |
//! | `On` | admitted | admitted | admitted | its own checks |
//! | `ExitOnly` (switched off since) | refused | refused | only if no call carries FANG | its own checks |
//! | `Paused` | refused | refused | refused | its own checks (it renews the pause) |
//!
//! So while programs are dormant every program action is refused, whoever signs it: that's how v2
//! stays v1 until programs switch on, since `verify` rejects a block with an unadmittable
//! transaction. A `ProgramsPause`'s own checks (§2.5) are its signer's (a validator of the epoch's
//! active set, from its own account or its owner's) and §7.2's limits (the cap, a live vote, the
//! window), all here, so a signature they refuse is never charged.
//!
//! Then the action's checks against the state (§2.2–2.4), each a precise refusal: what it names
//! exists and is usable (code from the block after its upload, a program from the block after its
//! creation, A5: a sender can't queue a create behind its own upload, nor a call behind its own
//! create), at the version the signer was shown, with methods of the right kind; and last its fuel
//! (§5.4): `fuel_limit` within `program_max_tx_fuel`, the code it loads within `MAX_TX_CODE_BYTES`,
//! its reservation no larger than a block, and, in `Block` mode, within what's left of the block's
//! budget (a mempool simulation isn't a block). Everything here is map reads: interfaces are held
//! decoded in their records, so admission never parses one (§6.4).

use super::fuel::{self, parse_hash};
use super::params::{param, Param};
use super::state::{blob_key, ProgramRecord, BLOB_CODE, BLOB_INTERFACE};
use super::{digest, is_program, pause, status, Status};
use crate::chain::{short, ApplyMode, State};
use crate::types::{Action, Binding, InitCall, Tx};
use anyhow::{anyhow, bail, ensure, Result};
use blockula_vm::config::MAX_TX_CODE_BYTES;
use blockula_vm::exec::ANY_VERSION;
use blockula_vm::host::MethodKind;
use blockula_vm::interface::Interface;

/// Refuses `tx`, at `height`, if programs' rules don't let it in (the module docs). `mode` decides
/// only whether the block's fuel budget is checked (`Block`) or not (`Mempool`).
pub fn admit(state: &State, tx: &Tx, height: u64, mode: ApplyMode) -> Result<()> {
    let status = status(state, height);
    // Gated on the status, not on "no program exists", like every hook of §1.2: while programs
    // are dormant no program does.
    if status != Status::Dormant && is_program(state, &tx.from) {
        bail!("{} is a program: programs have no keys, so no transaction acts from one", short(&tx.from));
    }
    // A parameter of programs' is unknown, as in v1, until programs exist (§4.5).
    super::params::admit(state, &tx.action, height)?;
    if !tx.action.is_program() {
        return Ok(());
    }
    refuse_by_status(status, &tx.action)?;
    // In any status but dormant, a pause running included (a signature then counts toward renewing
    // it), a pause runs no program and reserves no fuel: only its signer and §7.2's limits.
    if let Action::ProgramsPause {} = tx.action {
        return check_pause(state, tx, height);
    }
    match &tx.action {
        Action::CreateProgram { .. } => check_create(state, tx, height)?,
        Action::CallProgram { .. } => check_call(state, tx, height)?,
        // An upload depends on nothing else in the state (§2.2): what it stores is checked when it
        // runs, on the proposer's trial.
        _ => {}
    }
    check_fuel(state, tx, height, mode)
}

/// What the [`Status`] alone refuses of the program action `action` (the table in the module docs):
/// everything while dormant, then every tag but the pause while paused, uploads and creates and
/// calls that carry FANG in exit-only. Whatever is left (`Ok`) depends on the signer or the state:
/// [`admit`]'s other checks. The one place those rules are written: admission runs it, and so does
/// `submit_inner`'s early refusal ([`refuse_early`]).
fn refuse_by_status(status: Status, action: &Action) -> Result<()> {
    let kind = action.kind();
    match (status, action) {
        (Status::Dormant, _) => bail!("programs are off: a {kind} transaction is refused until protocol v2 is in force and governance switches the `programs` feature on"),
        // In any other status, a pause running included.
        (_, Action::ProgramsPause {}) => {}
        (Status::Paused { until }, _) => bail!("programs are paused until block {until}: a {kind} transaction is refused until then"),
        (Status::ExitOnly, Action::UploadCode { .. } | Action::CreateProgram { .. }) => bail!("programs are exit-only (governance switched them off): a {kind} transaction is refused; only calls that send programs no FANG run"),
        (Status::ExitOnly, Action::CallProgram { calls, .. }) if calls.iter().any(|c| c.value > 0) => bail!("programs are exit-only (governance switched them off): a call can't send a program FANG"),
        _ => {}
    }
    Ok(())
}

/// `submit_inner`'s early refusal (§8.2): `tx`, if it's a program transaction, against programs'
/// status at the height the next block has (`econ::now`), before its fee or its signature are
/// checked, as Ghost Mode's is. Exactly what [`admit`] refuses by status ([`refuse_by_status`]),
/// tag by tag: 62 and 63 unless `On`; 64 unless `On`, or exit-only with every call's `value` 0; 69
/// while dormant; every one of them while paused but 69. So exits and pauses get through the
/// public node while programs are exit-only or paused, and a transaction no block could include
/// costs the node neither a signature check nor a turn at the chain lock.
///
/// It decides nothing that depends on the signer or the state (whose key, which program exists):
/// those need what the sender's own queued transactions may change (a queued `AddKey`, a queued
/// create), so they stay with admission, which runs after the signature check, on the sender's
/// queue. Every tx but a program's is `Ok`.
pub fn refuse_early(state: &State, tx: &Tx) -> Result<()> {
    if !tx.action.is_program() {
        return Ok(());
    }
    refuse_by_status(status(state, crate::econ::now(state)), &tx.action)
}

/// A `ProgramsPause`'s checks (§2.5): the validator its signature counts for, one of the epoch's
/// active set signing from its own account or its owner's ([`pause::validator_of`]), and §7.2's
/// limits on it ([`pause::may_sign`]: the cap, a live vote, the window), so a signature they refuse
/// is never charged.
fn check_pause(state: &State, tx: &Tx, height: u64) -> Result<()> {
    let validator = pause::validator_of(state, &tx.from, &tx.signer_key_id()?, height)?;
    pause::may_sign(state, &validator, height)
}

/// A `CreateProgram`'s checks against the state (§2.3), but its fuel's.
fn check_create(state: &State, tx: &Tx, height: u64) -> Result<()> {
    let Action::CreateProgram { wasm_hash, interface_hash, authority, upgrade_delay_blocks, init, bindings, .. } = &tx.action else { return Ok(()) };
    // Both blobs stored, usable at `height`, and matching: the interface's methods are the code's
    // exports (they may come from different uploads).
    let code_hash = parse_hash(wasm_hash).ok_or_else(|| anyhow!("`wasm_hash` isn't a hash"))?;
    let interface_hash = parse_hash(interface_hash).ok_or_else(|| anyhow!("`interface_hash` isn't a hash"))?;
    let code = state.programs.code.get(&code_hash).ok_or_else(|| anyhow!("code {} isn't stored: upload it first", short_hash(&code_hash)))?;
    usable_blob(state, BLOB_CODE, &code_hash, height)?;
    let interface = stored_interface(state, &interface_hash)?;
    usable_blob(state, BLOB_INTERFACE, &interface_hash, height)?;
    blockula_vm::interface::check_exports(interface, &code.exports).map_err(|m| anyhow!("interface {} doesn't match code {}: {m}", short_hash(&interface_hash), short_hash(&code_hash)))?;
    check_init(interface, init.as_ref())?;
    if authority.is_some() {
        let least = param(state, Param::MinUpgradeDelayBlocks);
        ensure!(*upgrade_delay_blocks >= least, "an upgradable program gives at least {least} blocks' notice of an upgrade (program_min_upgrade_delay_blocks), not {upgrade_delay_blocks}");
    }
    check_bindings(state, bindings, height)?;
    // The address it creates (from its sender and this tx's nonce, §2.1) is free, or holds only a
    // balance sent ahead, which the program adopts: no keys, no locks, nonce 0.
    let creator = crate::crypto::address_bytes(&tx.from).ok_or_else(|| anyhow!("`from` isn't an address"))?;
    let address = digest::program_address(state.params.prefix(), &creator, tx.nonce);
    ensure!(!is_program(state, &address), "the address this create makes, {}, is a program already", short(&address));
    if let Some(a) = state.accounts.get(&address) {
        ensure!(a.keys.is_empty() && a.locks.is_empty() && a.nonce == 0, "the address this create makes, {}, holds an account a program can't adopt (it has keys, locks or a nonce)", short(&address));
    }
    Ok(())
}

/// `init` is present exactly when the interface declares an `Init` method, names it, and sends it
/// FANG only if it's payable (§2.3). Method names are the deployer's: refusals don't quote them.
/// The create handler re-checks it in the action phase (§6.1 step 1).
pub(super) fn check_init(interface: &Interface, init: Option<&InitCall>) -> Result<()> {
    match (interface.init(), init) {
        (None, None) => Ok(()),
        (None, Some(_)) => bail!("the interface declares no initializer, so the create can't carry `init`"),
        (Some(_), None) => bail!("the interface declares an initializer, so the create must run it (`init`)"),
        (Some(m), Some(i)) => {
            ensure!(i.method == m.name, "`init.method` isn't the interface's initializer");
            ensure!(i.value == 0 || m.payable, "the interface's initializer isn't payable, so `init.value` must be 0");
            Ok(())
        }
    }
}

/// A `CallProgram`'s checks against the state (§2.4), but its fuel's.
fn check_call(state: &State, tx: &Tx, height: u64) -> Result<()> {
    let Action::CallProgram { calls, bindings, .. } = &tx.action else { return Ok(()) };
    for (n, c) in calls.iter().enumerate().map(|(i, c)| (i + 1, c)) {
        let record = usable_program(state, &c.program, height)?;
        let in_force = version_in_force(&c.program, record);
        ensure!(c.version_digest == in_force, "call {n} names version {} of program {}, whose version in force is {}", short_hex(&c.version_digest), short(&c.program), short_hex(&in_force));
        let interface = stored_interface(state, &record.interface_hash)?;
        let Some(method) = interface.method(&c.method) else { bail!("call {n}: program {}'s interface has no method by that name", short(&c.program)) };
        match method.kind {
            MethodKind::Call => {}
            MethodKind::View => bail!("call {n}: that method of program {} is a view, which no transaction calls", short(&c.program)),
            MethodKind::Init => bail!("call {n}: that method of program {} is its initializer, which runs only when the program is created", short(&c.program)),
        }
        ensure!(c.value == 0 || method.payable, "call {n}: that method of program {} isn't payable, so the call can't send it FANG", short(&c.program));
    }
    check_bindings(state, bindings, height)
}

/// Each binding names a program usable at `height`, and a digest other than "any version" (64
/// zeros) is its version in force (§2.3, §2.4).
fn check_bindings(state: &State, bindings: &[Binding], height: u64) -> Result<()> {
    let any = hex::encode(ANY_VERSION);
    for b in bindings {
        let record = usable_program(state, &b.program, height)?;
        let in_force = version_in_force(&b.program, record);
        ensure!(b.version_digest == any || b.version_digest == in_force, "a binding names version {} of program {}, whose version in force is {}", short_hex(&b.version_digest), short(&b.program), short_hex(&in_force));
    }
    Ok(())
}

/// The fuel checks (§2.3, §2.4, §5.4): `fuel_limit` within `program_max_tx_fuel` (an upload signs
/// none: its `exec` is held to the block's budget only), the distinct code it loads within
/// `MAX_TX_CODE_BYTES` (which bounds `load`, so the reservation is within `MAX_RESERVATION`), the
/// reservation no larger than a block in every mode, and in `Block` mode within what the block at
/// `height` has left.
fn check_fuel(state: &State, tx: &Tx, height: u64, mode: ApplyMode) -> Result<()> {
    if let Action::CreateProgram { fuel_limit, .. } | Action::CallProgram { fuel_limit, .. } = &tx.action {
        let max = param(state, Param::MaxTxFuel);
        ensure!(*fuel_limit <= max, "`fuel_limit` {fuel_limit} is above program_max_tx_fuel ({max})");
    }
    let code: usize = fuel::declared_code(state, tx).values().sum();
    ensure!(code <= MAX_TX_CODE_BYTES, "the code this transaction declares is {code} bytes, past the {MAX_TX_CODE_BYTES} one transaction may load");
    let reserved = fuel::reserved(state, tx, height);
    let max_block = param(state, Param::MaxBlockFuel);
    ensure!(reserved <= max_block, "larger than a block: this transaction reserves {reserved} fuel, and a block counts at most {max_block} (program_max_block_fuel)");
    if mode == ApplyMode::Block {
        let used = state.programs.block.fuel_at(height);
        ensure!(used.saturating_add(reserved) <= max_block, "the block's program fuel is spent: this transaction reserves {reserved} fuel, and block {height} has counted {used} of its {max_block}");
    }
    Ok(())
}

/// The program at `address`, if it's usable at `height`: it exists, and was created in an earlier
/// block (A5; every v0 program stays active, nothing closes one).
fn usable_program<'a>(state: &'a State, address: &str, height: u64) -> Result<&'a ProgramRecord> {
    let record = state.programs.records.get(address).ok_or_else(|| anyhow!("{} isn't a program", short(address)))?;
    ensure!(record.created_at < height, "program {} was created at block {}: it's usable from block {}", short(address), record.created_at, record.created_at + 1);
    Ok(record)
}

/// A stored code or interface (its blob reference) is usable at `height`: uploaded in an earlier
/// block (A5). The create handler re-checks it in the action phase (§6.1 step 1).
pub(super) fn usable_blob(state: &State, kind: u8, hash: &[u8; 32], height: u64) -> Result<()> {
    let what = if kind == BLOB_CODE { "code" } else { "interface" };
    let blob = state.programs.blob_refs.get(&blob_key(kind, hash)).ok_or_else(|| anyhow!("{what} {} isn't stored: upload it first", short_hash(hash)))?;
    ensure!(blob.uploaded_at < height, "{what} {} was uploaded at block {}: it's usable from block {}", short_hash(hash), blob.uploaded_at, blob.uploaded_at + 1);
    Ok(())
}

/// A stored interface, decoded (its record holds it so).
fn stored_interface<'a>(state: &'a State, hash: &[u8; 32]) -> Result<&'a Interface> {
    let record = state.programs.interfaces.get(hash).ok_or_else(|| anyhow!("interface {} isn't stored: upload it first", short_hash(hash)))?;
    // Every interface an upload stores decoded at its door (`check_format`), so this never fails.
    record.interface().ok_or_else(|| anyhow!("stored interface {} doesn't decode", short_hash(hash)))
}

/// The version digest a call to the program at `address` must name (§2.1), as lowercase hex.
fn version_in_force(address: &str, record: &ProgramRecord) -> String {
    hex::encode(digest::version_digest(address, &record.wasm_hash, &record.interface_hash, record.version))
}

/// A hash as refusals show it: its first 10 hex characters.
fn short_hash(hash: &[u8; 32]) -> String {
    short_hex(&hex::encode(hash))
}

/// A hex digest's first 10 characters (by character, so a malformed one can't split one).
fn short_hex(h: &str) -> String {
    format!("{}…", h.chars().take(10).collect::<String>())
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod e2e_tests;
#[cfg(test)]
mod early_tests;
