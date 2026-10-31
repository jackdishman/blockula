//! The chain as a program transaction's execution reads it (docs/PROGRAMS-V0.md §6.1 step 2):
//! [`ProgramView`], W1's `vm::host::View` over the state the action phase sees, that is **after
//! the transaction's fee phase** (its charge debited, its tip paid, its nonce bumped), so the
//! balance the VM holds `value` and deposits to is the one commit's `j_debit` finds.
//!
//! The view answers for the transaction's **declared set** only: the programs its calls name and
//! its bindings (for a create, the new program and its bindings), each resolved once from its
//! record when the view is made: its code in force, its decoded interface and its version digest.
//! The VM reaches no other program (`TrapKind::UnboundProgram` before it asks), and loads exactly
//! the code [`ProgramView::codes`] lists. A declared address that isn't a program reads as none
//! (`TrapKind::NotAProgram`), and so does a program created in this block (A5: callable from the
//! block after its creation), except the one the create running now is making, whose `init` runs.
//! (Admission refuses both first, WBS S3; the view's answer doesn't rest on it.)
//!
//! Accounts read as the state holds them, with one addition (A30): a keyless system account
//! (`chain::system_account`) that has no record yet reads as an existing account holding 0, so a
//! program pays no creation fee to "open" the treasury, as no native action does.
//!
//! Every answer is a function of the state and the height, so every node gets the same ones.

use super::digest;
use super::exec::{Faulted, Run};
use super::params::{param, Param};
use super::state::storage_key;
use crate::chain::{system_account, State};
use crate::crypto;
use blockula_vm::engine::{Address, CodeHash, Digest};
use blockula_vm::exec::Entry;
use blockula_vm::host::{MethodInfo, View};
use blockula_vm::interface::Interface;
use std::collections::BTreeMap;

/// A program of the declared set that a call may reach now, as its record has it.
struct Declared<'s> {
    /// Its code in force (`wasm_hash`), the hash the engine loads it under.
    code: CodeHash,
    /// Its interface, as its record holds it decoded (the kinds and payability `View::method`
    /// answers from).
    interface: &'s Interface,
    /// Its version digest in force (§2.1).
    digest: Digest,
}

/// W1's `View` over `&State` for one program transaction (the module docs).
pub struct ProgramView<'s> {
    state: &'s State,
    height: u64,
    declared: BTreeMap<Address, Declared<'s>>,
    /// Test builds: a fault a test injected for this transaction ([`inject`]).
    #[cfg(test)]
    injected: Option<inject::Injected>,
}

impl<'s> ProgramView<'s> {
    /// The view `run`'s transaction runs against: `state` after its fee phase, at its height,
    /// answering for `declared` (in any order; repeats are one program): each that's a program
    /// created before this block, and `creating`, the program a create is making, already
    /// recorded in `state` (its `init` runs although its `created_at` is this block).
    ///
    /// Stored records that don't hold together (a program citing code or an interface that isn't
    /// stored, or an interface whose bytes don't decode) are a node fault tied to the transaction:
    /// upload stored them only after checking them, so no valid transaction meets one.
    pub fn new(state: &'s State, run: &Run, declared: impl IntoIterator<Item = Address>, creating: Option<Address>) -> Result<ProgramView<'s>, Faulted> {
        Self::at(state, run.height, &run.tx, declared, creating)
    }

    /// [`ProgramView::new`] for a run that isn't a transaction's (`/view`, [`super::view_api`]):
    /// `height` is the height the run judges at, and `tx` names it, for the tests' injected faults.
    pub(super) fn at(state: &'s State, height: u64, tx: &str, declared: impl IntoIterator<Item = Address>, creating: Option<Address>) -> Result<ProgramView<'s>, Faulted> {
        // Only the tests' injected faults ask what the run is called.
        #[cfg(not(test))]
        let _ = tx;
        let prefix = state.params.prefix();
        let mut resolved = BTreeMap::new();
        for program in declared {
            if resolved.contains_key(&program) {
                continue;
            }
            let address = crypto::render_address(prefix, &program);
            let Some(record) = state.programs.records.get(&address) else { continue };
            if record.created_at >= height && creating != Some(program) {
                continue;
            }
            if !state.programs.code.contains_key(&record.wasm_hash) {
                return Err(Faulted::tx(format!("program {address} cites code {} that isn't stored", hex::encode(record.wasm_hash))));
            }
            let stored = state.programs.interfaces.get(&record.interface_hash).ok_or_else(|| Faulted::tx(format!("program {address} cites interface {} that isn't stored", hex::encode(record.interface_hash))))?;
            let interface = stored.interface().ok_or_else(|| Faulted::tx(format!("program {address}'s stored interface doesn't decode")))?;
            let digest = digest::version_digest(&address, &record.wasm_hash, &record.interface_hash, record.version);
            resolved.insert(program, Declared { code: record.wasm_hash, interface, digest });
        }
        #[cfg(test)]
        inject::observe(tx);
        Ok(ProgramView {
            state,
            height,
            declared: resolved,
            #[cfg(test)]
            injected: inject::armed_for(tx, state.programs.node),
        })
    }

    /// The distinct code the engine must load before anything runs: the declared programs', by
    /// hash (so in hash order, §6.1 step 4), with its bytes (stored: `new` checked).
    pub fn codes(&self) -> BTreeMap<CodeHash, &'s [u8]> {
        let code = &self.state.programs.code;
        self.declared.values().filter_map(|d| code.get(&d.code).map(|c| (d.code, c.wasm.as_slice()))).collect()
    }

    /// Test builds: a view a test armed with [`inject::Injected::Panic`] panics here, on the
    /// program thread, inside the VM (a `Tx` fault, §6.2).
    #[cfg(test)]
    fn hook(&self) {
        if self.injected == Some(inject::Injected::Panic) {
            panic!("a fault injected into the program view (test)");
        }
    }
}

impl View for ProgramView<'_> {
    fn height(&self) -> u64 {
        #[cfg(test)]
        self.hook();
        self.height
    }

    fn balance(&self, account: &Address) -> Option<u64> {
        let prefix = self.state.params.prefix();
        let address = crypto::render_address(prefix, account);
        match self.state.accounts.get(&address) {
            Some(a) => Some(a.balance),
            // A30: a keyless system account exists for programs, as it does for native actions.
            None => system_account(prefix, &address).map(|_| 0),
        }
    }

    fn code_of(&self, program: &Address) -> Option<CodeHash> {
        #[cfg(test)]
        self.hook();
        self.declared.get(program).map(|d| d.code)
    }

    fn version_digest(&self, program: &Address) -> Option<Digest> {
        self.declared.get(program).map(|d| d.digest)
    }

    fn method(&self, program: &Address, name: &str) -> Option<MethodInfo> {
        self.declared.get(program).and_then(|d| d.interface.method(name))
    }

    fn storage(&self, program: &Address, key: &[u8]) -> Option<Entry> {
        let e = self.state.programs.storage.get(&storage_key(program, key))?;
        Some(Entry { value: e.value.clone(), payer: e.payer, deposit: e.deposit, capacity: e.capacity })
    }

    fn state_bytes(&self) -> u64 {
        self.state.programs.meta.as_ref().map_or(0, |m| m.state_bytes)
    }

    fn storage_price(&self, used: u64) -> Option<u64> {
        storage_price(self.state, used)
    }

    fn account_creation_fee(&self) -> u64 {
        self.state.chain_params.account_creation_fee
    }
}

/// Drops per accounted byte a deposit bonds when it brings program state to `used` bytes, or
/// `None` past the ceiling (§4.6): W1's `storage::price` over `program_storage_price` and
/// `program_state_soft_bytes`, and nothing else. Entries (through [`ProgramView`]), upload blobs and
/// program records are all priced here, each at the bytes once it lands.
pub fn storage_price(state: &State, used: u64) -> Option<u64> {
    blockula_vm::storage::price(param(state, Param::StoragePrice), param(state, Param::StateSoftBytes), used)
}

/// Fault injection for tests (WBS S4a's hook, which S4b's fault paths use): a test arms a fault
/// for one transaction, by its id, and the program transaction that runs it meets the fault where
/// a real one would arise, on every path that runs it (a proposer's run, verification,
/// execution). Process-wide, so the program thread sees it; keyed by transaction id, so tests that
/// run in parallel don't meet each other's. A fault can also be armed for some simulated nodes
/// only ([`arm_on`]): the simulator tags each node's state (`Programs::node`), and a view asks
/// for the fault of its own state's node.
#[cfg(test)]
pub mod inject {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// What a test injects.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Injected {
        /// The view panics inside the VM (`NodeFault::Panic` from `execute`: a `Tx` fault).
        Panic,
        /// The OS refuses this many program-thread spawns, the transaction's next ones (three
        /// refused in a row are a `Setup` fault, §6.1 step 3).
        RefuseSpawns(u32),
    }

    static ARMED: Mutex<BTreeMap<String, Injected>> = Mutex::new(BTreeMap::new());

    /// Faults armed for some simulated nodes only: transaction id → the fault and the nodes
    /// (`Programs::node`) that meet it.
    static ARMED_ON: Mutex<BTreeMap<String, (Injected, Vec<usize>)>> = Mutex::new(BTreeMap::new());

    /// Files to read when a transaction's program view is made (inside its trial), and what each
    /// held then: a test's way to see what a path wrote before running a program transaction.
    static LOOKS: Mutex<Looks> = Mutex::new(BTreeMap::new());

    /// Transaction id → the file to read and what it held.
    type Looks = BTreeMap<String, (std::path::PathBuf, Option<Vec<u8>>)>;

    /// The file at `path` is read when the program transaction `tx_id` builds its view, until
    /// [`looked`] takes what it read.
    pub fn look(tx_id: &str, path: std::path::PathBuf) {
        LOOKS.lock().unwrap_or_else(|e| e.into_inner()).insert(tx_id.to_string(), (path, None));
    }

    /// What the file held when `tx_id`'s view was made: `None` if it never was (or the file didn't
    /// exist). Ends the look.
    pub fn looked(tx_id: &str) -> Option<Vec<u8>> {
        LOOKS.lock().unwrap_or_else(|e| e.into_inner()).remove(tx_id).and_then(|(_, seen)| seen)
    }

    /// The view of `tx_id` is being made: reads the file a test asked to look at.
    pub(super) fn observe(tx_id: &str) {
        if let Some((path, seen)) = LOOKS.lock().unwrap_or_else(|e| e.into_inner()).get_mut(tx_id) {
            *seen = std::fs::read(path).ok();
        }
    }

    fn armed_map() -> std::sync::MutexGuard<'static, BTreeMap<String, Injected>> {
        ARMED.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Arms `what` for the transaction `tx_id`, until [`disarm`].
    pub fn arm(tx_id: &str, what: Injected) {
        armed_map().insert(tx_id.to_string(), what);
    }

    pub fn disarm(tx_id: &str) {
        armed_map().remove(tx_id);
        ARMED_ON.lock().unwrap_or_else(|e| e.into_inner()).remove(tx_id);
    }

    /// Arms `what` for the transaction `tx_id` on the simulated `nodes` only (the views of states
    /// tagged with one of them), until [`disarm`].
    pub fn arm_on(tx_id: &str, what: Injected, nodes: &[usize]) {
        ARMED_ON.lock().unwrap_or_else(|e| e.into_inner()).insert(tx_id.to_string(), (what, nodes.to_vec()));
    }

    /// What's armed for `tx_id` on the node whose state is tagged `node`: a fault armed for every
    /// node, or one armed for that node.
    pub fn armed_for(tx_id: &str, node: Option<usize>) -> Option<Injected> {
        armed(tx_id).or_else(|| {
            let on = ARMED_ON.lock().unwrap_or_else(|e| e.into_inner());
            on.get(tx_id).filter(|(_, nodes)| node.is_some_and(|n| nodes.contains(&n))).map(|(what, _)| *what)
        })
    }

    /// What's armed for `tx_id`.
    pub fn armed(tx_id: &str) -> Option<Injected> {
        armed_map().get(tx_id).copied()
    }

    /// Whether `tx_id` is armed to have its spawns refused, not counting one down: a block pass's
    /// thread meets the same OS refusal before the transaction's own does (`pass::block_pass`).
    pub fn refusing(tx_id: &str) -> bool {
        matches!(armed_map().get(tx_id), Some(Injected::RefuseSpawns(n)) if *n > 0)
    }

    /// Whether the next spawn for `tx_id` is refused (counting the refusal down).
    pub fn refuse_spawn(tx_id: &str) -> bool {
        let mut map = armed_map();
        match map.get_mut(tx_id) {
            Some(Injected::RefuseSpawns(n)) if *n > 0 => {
                *n -= 1;
                true
            }
            _ => false,
        }
    }
}
