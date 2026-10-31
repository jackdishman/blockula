//! `CreateProgram` (tag 63) in a block (docs/PROGRAMS-V0.md §2.3, "Execution"): a program from
//! stored code and interface, at the address its creator's transaction nonce derives (§2.1),
//! callable from the block after (A5).
//!
//! 0. **The blobs and `init`**, admission's checks re-checked in its words (§6.1 step 1): both
//!    blobs stored, each uploaded in an earlier block (A5), the pair matching, and `init` present
//!    exactly when the interface declares an initializer (naming it, with value only if it's
//!    payable).
//! 1. **The address.** If an account exists there it must have no keys, no locks and nonce 0 (FANG
//!    someone sent ahead): it's adopted, its balance joining `value_held` (§7.1). Otherwise the
//!    create fails. Adoption keeps a balance sent ahead from blocking the create; it's no way to
//!    fund a program: a create that fails spends its nonce, so nothing reaches that address again
//!    and the balance stays there for good.
//! 2. **The record deposit**: `RECORD_BYTES` at the price once the record lands, bonded from the
//!    creator, counted against `storage_limit` (A7), so `init`'s writes get what's left.
//! 3. **The record** (inert authority, policy and delay as signed, A19), META's counts, a reference
//!    on each blob, and the program's account, opened if it's absent (`econ::charge_new_accounts`
//!    then burns the creation fee from the creator, as for any account an action opens).
//! 4. **`init`**, when present: one top-level call of kind `Init` to the new program, with its
//!    value, the bindings as the declared set, `fuel_limit` and the remaining storage limit. Any
//!    failure fails the whole create: the record, the account, the deposits and the value all roll
//!    back with the action journal, and only the fee sticks.
//! 5. **Commit** its diff, then the value cap on the FANG programs control, which an adopted
//!    balance and `init.value` raise.

use super::digest::{self, FIRST_VERSION};
use super::exec::{self, Failure, Faulted, Run, Stop};
use super::receipt::{self, OutsideVm, StorageSummary};
use super::state::{blob_key, meta_mut, ProgramRecord, Versioned, BLOB_CODE, BLOB_INTERFACE};
use super::view::{storage_price, ProgramView};
use super::{admit, commit, is_program, RECORD_BYTES};
use crate::chain::{short, Journal, State};
use crate::crypto;
use crate::types::{Action, ProgramPolicy, Tx};
use blockula_vm::exec::{Call, TxLimits};
use blockula_vm::host::MethodKind;
use blockula_vm::interface;

/// Applies `tx`, a `CreateProgram` from `sender`, in the block at `run.height`.
pub fn apply(state: &mut State, j: &mut Journal, run: &Run, tx: &Tx, sender: &str) -> Result<(), Stop> {
    let Action::CreateProgram { wasm_hash, interface_hash, authority, upgrade_delay_blocks, policy, accepts_transfers, init, fuel_limit, storage_limit, bindings } = &tx.action else {
        return Err(Failure::native("not a create").into());
    };
    let height = run.height;
    let before = commit::control(state);
    let (code, iface) = (super::call::hash(wasm_hash)?, super::call::hash(interface_hash)?);
    // The record cites both blobs, so they must be stored, usable from the block after their
    // upload (A5), and match: the interface's methods are the code's exports (they may come from
    // different uploads). Admission checks all three and `init` (§2.3); they're re-checked here
    // (§6.1 step 1) so that no record can cite blobs that aren't usable yet or a pair that doesn't
    // hold together, nor skip or misname its initializer, whichever path reaches this.
    let (Some(stored_code), Some(stored_iface)) = (state.programs.code.get(&code), state.programs.interfaces.get(&iface)) else {
        let missing = if state.programs.code.contains_key(&code) { ("interface", iface) } else { ("code", code) };
        return Err(Failure::native(format!("{} {} isn't stored", missing.0, hex::encode(missing.1))).into());
    };
    for (kind, hash) in [(BLOB_CODE, &code), (BLOB_INTERFACE, &iface)] {
        admit::usable_blob(state, kind, hash, height).map_err(Failure::native)?;
    }
    // Upload stored the interface only once it decoded, and its record holds it decoded: if it
    // holds none, this node differs.
    let decoded = stored_iface.interface().ok_or_else(|| Faulted::tx(format!("stored interface {} doesn't decode", hex::encode(iface))))?;
    interface::check_exports(decoded, &stored_code.exports).map_err(Failure::InterfaceMismatch)?;
    admit::check_init(decoded, init.as_ref()).map_err(Failure::native)?;
    let creator = crypto::address_bytes(sender).ok_or_else(|| Failure::native("the creator's address doesn't parse"))?;
    let program = digest::program_address_bytes(&creator, tx.nonce);
    let address = crypto::render_address(state.params.prefix(), &program);

    // 1. Absent, or adoptable.
    if is_program(state, &address) {
        return Err(Failure::native(format!("{} is a program already", short(&address))).into());
    }
    let (adopted, opens) = match state.accounts.get(&address) {
        None => (0, true),
        Some(a) if a.keys.is_empty() && a.locks.is_empty() && a.nonce == 0 => (a.balance, false),
        Some(_) => return Err(Failure::native(format!("{} has keys, locks or a nonce: a program can only adopt an account FANG was sent to", short(&address))).into()),
    };

    // 2. The record deposit, at the price once the record lands.
    let used = state.programs.meta.as_ref().map_or(0, |m| m.state_bytes).checked_add(RECORD_BYTES).ok_or(Failure::StateFull)?;
    let price = storage_price(state, used).ok_or(Failure::StateFull)?;
    let record_deposit = price.checked_mul(RECORD_BYTES).filter(|d| d <= storage_limit).ok_or(Failure::StorageLimit { deposits: u128::from(price) * u128::from(RECORD_BYTES), limit: *storage_limit })?;
    state.j_debit(j, sender, record_deposit).map_err(Failure::native)?;
    run.notes.borrow_mut().stored(StorageSummary { bytes_delta: RECORD_BYTES as i64, bonded: record_deposit, refunded: 0 });
    run.notes.borrow_mut().debited(OutsideVm { record_deposit, ..OutsideVm::default() });

    // 3. The record, META, the blobs' references and the account.
    let record = ProgramRecord {
        creator: sender.to_string(),
        created_at: height,
        wasm_hash: code,
        interface_hash: iface,
        version: FIRST_VERSION,
        authority: authority.clone(),
        upgrade_delay_blocks: *upgrade_delay_blocks,
        // The record's byte: 0 open, 1 additive (§4.2).
        policy: match policy {
            ProgramPolicy::Open => 0,
            ProgramPolicy::Additive => 1,
        },
        accepts_transfers: *accepts_transfers,
        items: 0,
        bytes: 0,
        deposit: 0,
        sponsored: 0,
        record_deposit,
        record_payer: sender.to_string(),
    };
    j.programs.records.touch(&state.programs.records, &address);
    state.programs.records.insert(address.clone(), Versioned(record));
    let m = meta_mut(state, j).ok_or_else(|| Failure::native("a create before programs exist"))?;
    let range = || Failure::native("program totals out of range");
    m.programs = m.programs.checked_add(1).ok_or_else(range)?;
    m.state_bytes = used;
    m.escrow = m.escrow.checked_add(record_deposit).ok_or_else(range)?;
    m.value_held = m.value_held.checked_add(adopted).ok_or_else(range)?;
    for key in [blob_key(BLOB_CODE, &code), blob_key(BLOB_INTERFACE, &iface)] {
        j.programs.blob_refs.touch(&state.programs.blob_refs, &key);
        let r = state.programs.blob_refs.get_mut(&key).ok_or_else(|| Failure::native("a stored blob without its reference"))?;
        r.refs = r.refs.checked_add(1).ok_or_else(range)?;
    }
    // Opens it if it's absent (a keyless account: no key hashes to a program's address).
    state.acct_mut(j, &address);
    run.notes.borrow_mut().created(address.clone());
    // The creation fee of that account is burned from the creator once this action has run
    // (`econ::charge_new_accounts`, which counts the accounts the action opened and no program's
    // commit did: this one). It's noted here, where it's known, since the receipt is made before
    // that burn; if the burn fails the transaction fails as `native` and its receipt drops it with
    // the rest of the action. An adopted account exists already: no fee.
    if opens {
        run.notes.borrow_mut().debited(OutsideVm { account_creation_fee: state.chain_params.account_creation_fee, ..OutsideVm::default() });
    }

    // 4–5. `init`, and its commit.
    if let Some(init) = init {
        let bindings = super::call::bindings(bindings)?;
        let call = Call {
            program,
            version_digest: digest::version_digest(&address, &code, &iface, FIRST_VERSION),
            method: init.method.clone(),
            input: init.input.0.clone(),
            value: init.value,
            kind: MethodKind::Init,
        };
        let declared = std::iter::once(program).chain(bindings.iter().map(|b| b.program));
        // What's left of `storage_limit` once the record deposit (at most it, step 2) is bonded.
        let limits = TxLimits { fuel_limit: *fuel_limit, storage_limit: storage_limit - record_deposit };
        let receipt = {
            let view = ProgramView::new(state, run, declared, Some(program))?;
            exec::run_calls(run, &view, creator, &[call], &bindings, limits)?
        };
        run.notes.borrow_mut().ran(&receipt);
        if let Some(fail) = receipt.fail {
            return Err(Failure::Vm(fail).into());
        }
        run.notes.borrow_mut().stored(receipt::storage_change(state, &receipt.diff));
        commit::apply_diff(state, j, &receipt.diff)?;
    }
    commit::check_control(state, before, height)?;
    Ok(())
}
