//! Program receipts (docs/PROGRAMS-V0.md §8.1): what a block did with each program transaction it
//! ran (tags 62, 63 and 64, applied or failed; never one it skipped, and never `ProgramsPause`,
//! whose effect is META state), as this node computed it. Display only (A10): no root commits to a
//! receipt and no snapshot holds one. `types::Block::programs` carries a block's receipts, by index
//! into its transactions, and the block log stores them with the block. Every node computes the
//! same ones from the same block (no clock, no map order, no engine text), so P2's replay gate,
//! which compares each replayed block's whole record, holds them to determinism across binaries
//! though they're no part of the root.
//!
//! **How a receipt is made.** `exec::Run::new` notes the transaction's fee parts and fuel figures
//! before its handler runs ([`Notes::new`]); the handler notes what it did as it goes (the VM's
//! receipt, the deposits it bonded and refunded, the program it created, the blobs it stored); and
//! `programs::apply` turns the notes and the outcome into a [`ProgramReceipt`] in the state's block
//! run ([`record`]). A failed transaction's receipt keeps what it paid and the fuel it burned, and
//! nothing else: nothing of its action happened. A node fault gets none (§6.2: never an outcome).
//! `Chain::apply_block` then takes the block's receipts ([`for_block`]): one whose program ran but
//! whose transaction then failed a native check says so (`native`), and the claims budget cuts the
//! block's claims at [`MAX_BLOCK_CLAIM_BYTES`]. `Chain::replay`, rebuilding the state from the
//! block log, keeps the logged receipts and checks the ones it computes again against them
//! ([`replayed`]).
//!
//! **Facts and claims** (PRG 8.4). The outcome, fuel, fee, balance changes, storage, the program
//! created and the blobs stored are facts: the state holds them too, and proves them
//! (`/api/proof`). Events and return data are the program's claims: the chain checks nothing in
//! them, and they cost 8 and 1 fuel a byte because they aren't meant to be kept, so a block records
//! at most [`MAX_BLOCK_CLAIM_BYTES`] of them.
//!
//! **Numbers, never engine text** (PRG 6.5). A failure carries §8.1's number (1–3 are W1's
//! `FailCode::number()`, 10 and up the node's) and W1's trap and validation rule numbers. The only
//! text a program puts in a receipt is its own abort message, which the VM sanitized
//! (`host::sanitize`, consensus), and its method and event names, which are identifiers.
//!
//! **JSON.** Amounts are strings of drops, as everywhere in the node's JSON (`types::u64_str`);
//! fuel, counts and the numbers of outcomes, traps and rules are JSON numbers (all below 2^53);
//! bytes are hex. [`ReceiptOutcome`] is `{"ok": true}`, or `{"ok": false, "number": n, "code":
//! "…"}` with its detail beside them.

use super::exec::{Failure, Run, Stop};
use super::state::BlockRun;
use super::{digest, fuel, is_program_tx};
use crate::chain::{short, State};
use crate::crypto;
use crate::types::{u64_str, Action, HexBytes, Tx};
use blockula_vm::engine::{Address, TrapKind};
use blockula_vm::exec::{CommitError, Diff, Event, FailCode, Receipt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The receipt format this binary writes.
pub const RECEIPT_VERSION: u8 = 1;

/// Most claims a block records over all its receipts, in bytes (§8.1): events (each its data, its
/// name and [`EVENT_CLAIM_BYTES`]) and return data (its length).
pub const MAX_BLOCK_CLAIM_BYTES: u64 = 256 << 10;

/// What the claims budget counts an event as beyond its name and data: its program's 32 address
/// bytes.
pub const EVENT_CLAIM_BYTES: u64 = 32;

/// One program transaction's receipt (the module docs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramReceipt {
    /// [`RECEIPT_VERSION`].
    pub version: u8,
    pub kind: ReceiptKind,
    pub outcome: ReceiptOutcome,
    pub fuel: FuelUse,
    /// What the transaction was charged, in parts.
    pub fee: FeeParts,
    /// The calls it made, in order: a call batch's, or a create's `init`. Each one's return data
    /// is empty unless the transaction applied.
    pub calls: Vec<CallResult>,
    /// The events its programs emitted, in order. Empty unless it applied.
    pub events: Vec<ReceiptEvent>,
    /// What the block's claims budget left out of this receipt, if it left anything out: then
    /// `events` and every call's return data are empty, and every fact is kept.
    pub omitted: Option<Omitted>,
    /// The net balance change of each account its programs moved FANG for (the VM's diff, the
    /// signer's own write bonds folded in), in the order of the accounts' 32 address bytes. The fee
    /// is in `fee`; the debits outside the VM are in `outside_vm`.
    pub balance_changes: Vec<BalanceChange>,
    /// What the transaction bonded (informational: the signer's own write bonds are in
    /// `balance_changes` too; [`OutsideVm`] has the debits that aren't).
    pub storage: StorageSummary,
    /// What the signer was debited outside the VM's diff, which `balance_changes` doesn't show.
    pub outside_vm: OutsideVm,
    /// Drops the VM's diff burned (`Diff::burned`): the creation fees of the accounts its programs'
    /// own sends opened, taken from the programs' balances inside the diff, never from the signer
    /// (`econ::charge_new_accounts` skips the accounts a program's commit opened). With it the
    /// receipt's facts conserve FANG: Σ `balance_changes` + (`storage.bonded` − `outside_vm.record_deposit`
    /// − `outside_vm.upload_deposit` − `storage.refunded`) + `burned_by_programs` = 0, a sum a
    /// client can assert instead of derive (§8.1). Additive: a receipt logged without it reads as 0.
    #[serde(default, with = "u64_str")]
    pub burned_by_programs: u64,
    /// The program a create made.
    pub created: Option<String>,
    /// What an upload stored.
    pub upload: Option<UploadResult>,
}

/// Which action a receipt is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptKind {
    Upload,
    Create,
    Call,
}

/// The fuel a transaction reserved, paid for and used (§5.1, §8.1).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuelUse {
    /// The signed `fuel_limit`: an upload's `upload_fuel` (it signs none), 0 for a create without
    /// `init`.
    pub limit: u64,
    /// The load fuel of the code it declared, each distinct code once.
    pub load: u64,
    /// W1's `Receipt::fuel_used`: exactly `limit` after any `OutOfFuel` that fails the transaction,
    /// nested or not, even when only a callee's smaller share ran out (under `try_call_v1` the
    /// caller runs on, and only the callee's share counts). An upload's is its `upload_fuel`; 0
    /// where no program ran (a create without `init`, or a failure before the VM). Nothing is
    /// refunded from it, and it isn't a measure of the work done.
    pub used: u64,
    /// `limit + load`: what the fuel fee was charged for.
    pub reserved: u64,
    /// The fuel rate it was charged at, in milli-drops per fuel.
    pub rate: u64,
}

/// A transaction's charge, in parts: its bytes' base fee, its fuel fee and its tip, in drops.
/// Their sum is what it was charged.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeeParts {
    #[serde(with = "u64_str")]
    pub bytes: u64,
    #[serde(with = "u64_str")]
    pub fuel: u64,
    #[serde(with = "u64_str")]
    pub tip: u64,
}

/// One call of a transaction: the program, the method (an identifier), the FANG it sent, and what
/// it returned (a claim).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallResult {
    pub program: String,
    pub method: String,
    #[serde(with = "u64_str")]
    pub value: u64,
    pub return_data: HexBytes,
}

/// An event a program emitted (a claim): the program, the event's name (an identifier) and data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptEvent {
    pub program: String,
    pub name: String,
    pub data: HexBytes,
}

/// The claims a receipt doesn't record because the block's budget had no room left for them: how
/// many events, their names' and data's bytes, and the return data's bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Omitted {
    pub events: u64,
    pub event_bytes: u64,
    pub return_bytes: u64,
}

/// An account's net balance change, in drops: negative for a debit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceChange {
    pub account: String,
    #[serde(with = "i128_str")]
    pub change: i128,
}

/// What a transaction did to program state's size and deposits: `bytes_delta` is the change in
/// META's `state_bytes`, and `bonded − refunded` the change in its `escrow`. A deposit is counted
/// against the entry stored before the transaction: a bond it kept (an overwrite that fits) moves
/// nothing. Informational: it says what was bonded, not whose balance paid it (a create's record
/// deposit and an upload's are in [`OutsideVm`], the signer's own write bonds in `balance_changes`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageSummary {
    pub bytes_delta: i64,
    #[serde(with = "u64_str")]
    pub bonded: u64,
    #[serde(with = "u64_str")]
    pub refunded: u64,
}

impl StorageSummary {
    /// This summary and `other` together (saturating: each is bounded by what program state can
    /// hold).
    fn add(&mut self, other: StorageSummary) {
        self.bytes_delta = self.bytes_delta.saturating_add(other.bytes_delta);
        self.bonded = self.bonded.saturating_add(other.bonded);
        self.refunded = self.refunded.saturating_add(other.refunded);
    }
}

/// What a transaction debited from its signer outside the VM's diff (§8.1), so that the signer's
/// balance reconciles exactly from the receipt: Δbalance(signer) = −(fee total + `total`) +
/// `balance_changes[signer]`. A create's record deposit, an upload's deposit and the account
/// creation fee a create burns when the program's address held no account (an adopted address pays
/// none). JSON: the three parts and their `total`, drops as strings; a `total` that isn't their sum
/// doesn't read back.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "OutsideVmJson", try_from = "OutsideVmJson")]
pub struct OutsideVm {
    pub record_deposit: u64,
    pub upload_deposit: u64,
    pub account_creation_fee: u64,
}

impl OutsideVm {
    /// The three parts together (saturating: no signer's balance holds more than `u64::MAX`).
    pub fn total(&self) -> u64 {
        self.record_deposit.saturating_add(self.upload_deposit).saturating_add(self.account_creation_fee)
    }
}

/// [`OutsideVm`]'s JSON.
#[derive(Serialize, Deserialize)]
struct OutsideVmJson {
    #[serde(with = "u64_str")]
    record_deposit: u64,
    #[serde(with = "u64_str")]
    upload_deposit: u64,
    #[serde(with = "u64_str")]
    account_creation_fee: u64,
    #[serde(with = "u64_str")]
    total: u64,
}

impl From<OutsideVm> for OutsideVmJson {
    fn from(o: OutsideVm) -> OutsideVmJson {
        OutsideVmJson { record_deposit: o.record_deposit, upload_deposit: o.upload_deposit, account_creation_fee: o.account_creation_fee, total: o.total() }
    }
}

impl TryFrom<OutsideVmJson> for OutsideVm {
    type Error = String;

    fn try_from(j: OutsideVmJson) -> Result<OutsideVm, String> {
        let o = OutsideVm { record_deposit: j.record_deposit, upload_deposit: j.upload_deposit, account_creation_fee: j.account_creation_fee };
        if o.total() != j.total {
            return Err(format!("outside_vm totals {}, but its parts add up to {}", j.total, o.total()));
        }
        Ok(o)
    }
}

/// What an upload stored: the blobs' hashes (hex), which of them were new, and the deposit it
/// bonded for them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadResult {
    pub wasm_hash: String,
    pub interface_hash: String,
    pub new_code: bool,
    pub new_interface: bool,
    #[serde(with = "u64_str")]
    pub deposit: u64,
}

/// A trap kind or a validation rule: its number (W1's, pinned there) and its name (the variant's,
/// pinned by `receipt::tests`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Numbered {
    pub number: u8,
    pub name: String,
}

impl Numbered {
    /// A fieldless enum's variant by its number and its name in snake case (`OutOfFuel` →
    /// `out_of_fuel`): W1 pins the numbers, and `receipt::tests` pins every name.
    fn of(number: u8, variant: impl std::fmt::Debug) -> Numbered {
        let mut name = String::new();
        for (i, c) in format!("{variant:?}").chars().enumerate() {
            if c.is_ascii_uppercase() && i > 0 {
                name.push('_');
            }
            name.push(c.to_ascii_lowercase());
        }
        Numbered { number, name }
    }
}

/// How a program transaction ended (§8.1's table): applied, or one of the numbered failures, each
/// with its detail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "OutcomeJson", try_from = "OutcomeJson")]
pub enum ReceiptOutcome {
    Ok,
    /// 1: a trap, and the program where the failure began.
    Trapped { program: String, trap: Numbered },
    /// 2: a program aborted, with its code and its own message (≤ 256 bytes, sanitized by the VM).
    Aborted { program: String, abort_code: u32, untrusted_message: String },
    /// 3: the diff failed the VM's commit checks (a host bug, counted by a metric).
    Commit { commit_error: u8 },
    /// 10: the FANG programs control would have passed the value cap: the cap, and the control
    /// (`value_held + sponsored`) the transaction would have left.
    ValueCap { cap: u64, control: u128 },
    /// 11: programs are exit-only, and the control would have risen.
    ExitOnly,
    /// 12: an upload's or a create's own deposits past its `storage_limit`.
    StorageLimit,
    /// 13: an upload or a create past program state's ceiling.
    StateFull,
    /// 14: upload validation refused the code: its rule, and where, when it says (never its
    /// message).
    InvalidCode { rule: Numbered, func: Option<u32>, offset: Option<u64> },
    /// 15: the test translation refused the code.
    TranslationRefused,
    /// 16: the interface's methods aren't the code's exports.
    InterfaceMismatch,
    /// 20: a native check failed (funds for `value` or deposits, locks, the creation fee): its
    /// wording is the transaction's error, as for any failed transaction.
    Native,
}

impl ReceiptOutcome {
    /// Whether the transaction applied.
    pub fn is_ok(&self) -> bool {
        *self == ReceiptOutcome::Ok
    }

    /// Its number (§8.1's table; 0 for `Ok`, which has none). Stable now, so a later receipts
    /// root can commit to it unchanged.
    pub fn number(&self) -> u8 {
        match self {
            ReceiptOutcome::Ok => 0,
            ReceiptOutcome::Trapped { .. } => 1,
            ReceiptOutcome::Aborted { .. } => 2,
            ReceiptOutcome::Commit { .. } => 3,
            ReceiptOutcome::ValueCap { .. } => 10,
            ReceiptOutcome::ExitOnly => 11,
            ReceiptOutcome::StorageLimit => 12,
            ReceiptOutcome::StateFull => 13,
            ReceiptOutcome::InvalidCode { .. } => 14,
            ReceiptOutcome::TranslationRefused => 15,
            ReceiptOutcome::InterfaceMismatch => 16,
            ReceiptOutcome::Native => 20,
        }
    }

    /// Its code (§8.1's table).
    pub fn code(&self) -> &'static str {
        match self {
            ReceiptOutcome::Ok => "ok",
            ReceiptOutcome::Trapped { .. } => "trapped",
            ReceiptOutcome::Aborted { .. } => "aborted",
            ReceiptOutcome::Commit { .. } => "commit",
            ReceiptOutcome::ValueCap { .. } => "value_cap",
            ReceiptOutcome::ExitOnly => "exit_only",
            ReceiptOutcome::StorageLimit => "storage_limit",
            ReceiptOutcome::StateFull => "state_full",
            ReceiptOutcome::InvalidCode { .. } => "invalid_code",
            ReceiptOutcome::TranslationRefused => "translation_refused",
            ReceiptOutcome::InterfaceMismatch => "interface_mismatch",
            ReceiptOutcome::Native => "native",
        }
    }

    /// The outcome of a transaction that failed for `failure`, naming programs under the chain's
    /// `prefix`.
    pub fn failed(failure: &Failure, prefix: &str) -> ReceiptOutcome {
        let program = |a: &Address| crypto::render_address(prefix, a);
        match failure {
            Failure::Vm(FailCode::Trapped { program: p, kind }) => ReceiptOutcome::Trapped { program: program(p), trap: Numbered::of(kind.number(), kind) },
            Failure::Vm(FailCode::Aborted { program: p, code, message }) => ReceiptOutcome::Aborted { program: program(p), abort_code: *code, untrusted_message: message.clone() },
            Failure::Vm(FailCode::Commit(e)) => ReceiptOutcome::Commit { commit_error: e.number() },
            Failure::ValueCap { cap, control } => ReceiptOutcome::ValueCap { cap: *cap, control: *control },
            Failure::ExitOnly => ReceiptOutcome::ExitOnly,
            Failure::StorageLimit { .. } => ReceiptOutcome::StorageLimit,
            Failure::StateFull => ReceiptOutcome::StateFull,
            Failure::InvalidCode(e) => ReceiptOutcome::InvalidCode { rule: Numbered::of(e.rule.number(), e.rule), func: e.func, offset: e.offset.map(|o| o as u64) },
            Failure::TranslationRefused => ReceiptOutcome::TranslationRefused,
            Failure::InterfaceMismatch(_) => ReceiptOutcome::InterfaceMismatch,
            Failure::Native(_) => ReceiptOutcome::Native,
        }
    }
}

/// [`ReceiptOutcome`]'s JSON: `ok`, and for a failure its number, code and detail, each detail
/// field present exactly for the codes that have it.
#[derive(Clone, Default, Serialize, Deserialize)]
struct OutcomeJson {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    number: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    program: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trap: Option<Numbered>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    abort_code: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    untrusted_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commit_error: Option<u8>,
    /// Drops, as a string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cap: Option<String>,
    /// Drops, as a string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    control: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rule: Option<Numbered>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    func: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    offset: Option<u64>,
}

impl From<ReceiptOutcome> for OutcomeJson {
    fn from(o: ReceiptOutcome) -> OutcomeJson {
        if o.is_ok() {
            return OutcomeJson { ok: true, ..OutcomeJson::default() };
        }
        let mut j = OutcomeJson { ok: false, number: Some(o.number()), code: Some(o.code().to_string()), ..OutcomeJson::default() };
        match o {
            ReceiptOutcome::Trapped { program, trap } => (j.program, j.trap) = (Some(program), Some(trap)),
            ReceiptOutcome::Aborted { program, abort_code, untrusted_message } => (j.program, j.abort_code, j.untrusted_message) = (Some(program), Some(abort_code), Some(untrusted_message)),
            ReceiptOutcome::Commit { commit_error } => j.commit_error = Some(commit_error),
            ReceiptOutcome::ValueCap { cap, control } => (j.cap, j.control) = (Some(cap.to_string()), Some(control.to_string())),
            ReceiptOutcome::InvalidCode { rule, func, offset } => (j.rule, j.func, j.offset) = (Some(rule), func, offset),
            ReceiptOutcome::Ok | ReceiptOutcome::ExitOnly | ReceiptOutcome::StorageLimit | ReceiptOutcome::StateFull | ReceiptOutcome::TranslationRefused | ReceiptOutcome::InterfaceMismatch | ReceiptOutcome::Native => {}
        }
        j
    }
}

impl TryFrom<OutcomeJson> for ReceiptOutcome {
    type Error = String;

    /// The outcome `j` describes, refusing one whose number isn't its code's or that lacks its
    /// code's detail: what `From` writes reads back as it was.
    fn try_from(j: OutcomeJson) -> Result<ReceiptOutcome, String> {
        if j.ok {
            return Ok(ReceiptOutcome::Ok);
        }
        let code = j.code.ok_or("a failed outcome without its code")?;
        let need = |what: &str| format!("a {code} outcome without its {what}");
        fn amount<T: std::str::FromStr>(s: Option<String>, code: &str, what: &str) -> Result<T, String> {
            s.ok_or_else(|| format!("a {code} outcome without its {what}"))?.parse().map_err(|_| format!("a {code} outcome's {what} isn't a number"))
        }
        let o = match code.as_str() {
            "trapped" => ReceiptOutcome::Trapped { program: j.program.ok_or_else(|| need("program"))?, trap: j.trap.ok_or_else(|| need("trap"))? },
            "aborted" => ReceiptOutcome::Aborted { program: j.program.ok_or_else(|| need("program"))?, abort_code: j.abort_code.ok_or_else(|| need("abort_code"))?, untrusted_message: j.untrusted_message.ok_or_else(|| need("untrusted_message"))? },
            "commit" => ReceiptOutcome::Commit { commit_error: j.commit_error.ok_or_else(|| need("commit_error"))? },
            "value_cap" => ReceiptOutcome::ValueCap { cap: amount(j.cap, &code, "cap")?, control: amount(j.control, &code, "control")? },
            "exit_only" => ReceiptOutcome::ExitOnly,
            "storage_limit" => ReceiptOutcome::StorageLimit,
            "state_full" => ReceiptOutcome::StateFull,
            "invalid_code" => ReceiptOutcome::InvalidCode { rule: j.rule.ok_or_else(|| need("rule"))?, func: j.func, offset: j.offset },
            "translation_refused" => ReceiptOutcome::TranslationRefused,
            "interface_mismatch" => ReceiptOutcome::InterfaceMismatch,
            "native" => ReceiptOutcome::Native,
            other => return Err(format!("an outcome of unknown code {other}")),
        };
        if j.number != Some(o.number()) {
            return Err(format!("a {code} outcome numbered {:?}, not {}", j.number, o.number()));
        }
        Ok(o)
    }
}

/// `i128` as a JSON string (a balance change in drops can pass 2^53).
mod i128_str {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &i128, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<i128, D::Error> {
        String::deserialize(d)?.parse().map_err(D::Error::custom)
    }
}

/// What a program transaction's run noted for its receipt (`exec::Run::notes`): its fee parts and
/// fuel figures from before its handler ran, then what the handler did. Read once the handler
/// returns ([`record`]); a failed transaction's receipt takes only the fuel it used from them.
pub struct Notes {
    fee: FeeParts,
    /// `used` stays 0 here: it's the VM's, or an upload's `limit` ([`Notes::receipt`]).
    fuel: FuelUse,
    /// The VM's run, once it ran: a call batch's, or a create's `init`.
    vm: Option<Ran>,
    storage: StorageSummary,
    outside: OutsideVm,
    created: Option<String>,
    upload: Option<UploadResult>,
}

/// What a receipt keeps of the VM's (W1's `exec::Receipt`).
struct Ran {
    fuel_used: u64,
    returns: Vec<Vec<u8>>,
    events: Vec<Event>,
    balances: Vec<(Address, i128)>,
    /// The diff's `burned`, which a commit refuses past a `u64` (saturated here: such a receipt is a failed one).
    burned: u64,
}

impl Notes {
    /// The notes of `tx`, executing at `height` on `state`, after its fee phase: what its fee
    /// phase charged it (its bytes' base fee, its fuel fee and its tip: `econ::charge`'s parts,
    /// which nothing of the fee phase moves), and the fuel it reserved (§5.1).
    pub fn new(state: &State, tx: &Tx, height: u64) -> Notes {
        let fee = FeeParts { bytes: crate::econ::base_fee(state, height, tx.size()), fuel: fuel::fuel_fee(state, tx, height), tip: tx.tip };
        let schedule = blockula_vm::meter::schedule(crate::upgrade::version_at(state, height));
        let (limit, load) = schedule.map_or((0, 0), |s| (fuel::exec(tx, s), fuel::load(state, tx, s)));
        let fuel = FuelUse { limit, load, used: 0, reserved: fuel::reserved(state, tx, height), rate: fuel::rate(state) };
        Notes { fee, fuel, vm: None, storage: StorageSummary::default(), outside: OutsideVm::default(), created: None, upload: None }
    }

    /// The VM ran the transaction's calls: `receipt` is W1's, failed or not.
    pub fn ran(&mut self, receipt: &Receipt) {
        let Receipt { fail: _, fuel_used, returns, events, diff } = receipt;
        self.vm = Some(Ran { fuel_used: *fuel_used, returns: returns.clone(), events: events.clone(), balances: diff.balances.clone(), burned: u64::try_from(diff.burned).unwrap_or(u64::MAX) });
    }

    /// The transaction moved program state's bytes and deposits by `change`.
    pub fn stored(&mut self, change: StorageSummary) {
        self.storage.add(change);
    }

    /// The transaction debited its signer `part` outside the VM's diff, where the handler takes it
    /// (the deposits), or will have burned it once the action ends (a create's account creation
    /// fee, `econ::charge_new_accounts`): added to what's noted already.
    pub fn debited(&mut self, part: OutsideVm) {
        self.outside.record_deposit = self.outside.record_deposit.saturating_add(part.record_deposit);
        self.outside.upload_deposit = self.outside.upload_deposit.saturating_add(part.upload_deposit);
        self.outside.account_creation_fee = self.outside.account_creation_fee.saturating_add(part.account_creation_fee);
    }

    /// A create made the program at `address`.
    pub fn created(&mut self, address: String) {
        self.created = Some(address);
    }

    /// An upload stored (or refreshed) its blobs.
    pub fn uploaded(&mut self, upload: UploadResult) {
        self.upload = Some(upload);
    }

    /// The receipt of `tx`, from `sender`, which applied (`failure` `None`) or failed, naming
    /// accounts under the chain's `prefix`.
    fn receipt(&self, prefix: &str, tx: &Tx, sender: &str, failure: Option<&Failure>) -> ProgramReceipt {
        let render = |a: &Address| crypto::render_address(prefix, a);
        let (kind, mut calls) = match &tx.action {
            Action::UploadCode { .. } => (ReceiptKind::Upload, vec![]),
            Action::CreateProgram { init, .. } => {
                // The program the create makes (§2.1), which `init` runs in, whether it applied or not.
                let program = crypto::address_bytes(sender).map(|c| digest::program_address(prefix, &c, tx.nonce)).unwrap_or_default();
                (ReceiptKind::Create, init.iter().map(|i| CallResult { program: program.clone(), method: i.method.clone(), value: i.value, return_data: HexBytes::default() }).collect())
            }
            Action::CallProgram { calls, .. } => (ReceiptKind::Call, calls.iter().map(|c| CallResult { program: c.program.clone(), method: c.method.clone(), value: c.value, return_data: HexBytes::default() }).collect()),
            _ => (ReceiptKind::Call, vec![]),
        };
        // An upload's fuel is its `upload_fuel`, whether its checks refused the code or not.
        let used = if kind == ReceiptKind::Upload { self.fuel.limit } else { self.vm.as_ref().map_or(0, |v| v.fuel_used) };
        let mut receipt = ProgramReceipt {
            version: RECEIPT_VERSION,
            kind,
            outcome: ReceiptOutcome::Ok,
            fuel: FuelUse { used, ..self.fuel.clone() },
            fee: self.fee.clone(),
            calls: vec![],
            events: vec![],
            omitted: None,
            balance_changes: vec![],
            storage: StorageSummary::default(),
            outside_vm: OutsideVm::default(),
            burned_by_programs: 0,
            created: None,
            upload: None,
        };
        if let Some(failure) = failure {
            receipt.outcome = ReceiptOutcome::failed(failure, prefix);
            receipt.calls = calls;
            return receipt;
        }
        if let Some(vm) = &self.vm {
            for (call, data) in calls.iter_mut().zip(&vm.returns) {
                call.return_data = HexBytes(data.clone());
            }
            receipt.events = vm.events.iter().map(|e| ReceiptEvent { program: render(&e.program), name: e.name.clone(), data: HexBytes(e.data.clone()) }).collect();
            receipt.balance_changes = vm.balances.iter().map(|(a, change)| BalanceChange { account: render(a), change: *change }).collect();
            receipt.burned_by_programs = vm.burned;
        }
        receipt.calls = calls;
        receipt.storage = self.storage.clone();
        receipt.outside_vm = self.outside.clone();
        receipt.created = self.created.clone();
        receipt.upload = self.upload.clone();
        receipt
    }
}

/// What committing `diff` does to program state's size and deposits (a [`StorageSummary`]), read
/// against `state` before it's committed: each changed entry against the one stored before. Called
/// with the VM's diff of a successful run, before `commit::apply_diff` writes it.
pub fn storage_change(state: &State, diff: &Diff) -> StorageSummary {
    let mut s = StorageSummary { bytes_delta: diff.state_bytes, bonded: 0, refunded: 0 };
    for change in &diff.storage {
        let old = state.programs.storage.get(&super::state::storage_key(&change.program, &change.key));
        let kept = matches!((old, &change.entry), (Some(o), Some(n)) if (o.payer, o.deposit, o.capacity) == (n.payer, n.deposit, n.capacity));
        if kept {
            continue;
        }
        if let Some(o) = old {
            s.refunded = s.refunded.saturating_add(o.deposit);
        }
        if let Some(n) = &change.entry {
            s.bonded = s.bonded.saturating_add(n.deposit);
        }
    }
    s
}

/// Records the receipt of `tx`, from `sender`, which `run` ran (`ran`: applied, failed or faulted)
/// in the block at `run.height`: `programs::apply`'s last step. Tags 62–64 only; a node fault gets
/// none (§6.2). The first receipt at a height starts that height's run (unjournaled, as receipts
/// are: a reader at that height takes another height's run as empty, so the reset changes no
/// answer).
pub fn record(state: &mut State, tx: &Tx, sender: &str, run: &Run, ran: &Result<(), Stop>) {
    if !is_program_tx(tx) {
        return;
    }
    let failure = match ran {
        Ok(()) => None,
        Err(Stop::Failed(failure)) => Some(failure),
        Err(Stop::Faulted(_)) => return,
    };
    let receipt = run.notes.borrow().receipt(state.params.prefix(), tx, sender, failure);
    let b = &mut state.programs.block;
    if b.height != run.height {
        *b = BlockRun { height: run.height, ..BlockRun::default() };
    }
    b.receipts.push((run.tx.clone(), receipt));
}

/// The receipts of the block at `height` that `state` has just run, taken from its block run and
/// keyed by index into `txs`, the transactions the block includes (`failed` the indices of those
/// that failed, as `types::Block` has them): `Chain::apply_block`'s, for the block it builds. A
/// receipt of a transaction the block skipped is dropped, and one whose program ran but whose
/// transaction then failed a native check (§6.1 step 6's last checks: creation fees, locks) becomes
/// `native`, and loses the facts and claims of an action that didn't happen. Then the claims budget
/// ([`MAX_BLOCK_CLAIM_BYTES`]). Empty for a block without program transactions, which computes
/// nothing (a v1 block among them).
pub fn for_block(state: &mut State, height: u64, txs: &[Tx], failed: &BTreeMap<usize, String>) -> BTreeMap<usize, ProgramReceipt> {
    let run = std::mem::take(&mut state.programs.block.receipts);
    if run.is_empty() || state.programs.block.height != height {
        return BTreeMap::new();
    }
    let at: BTreeMap<String, usize> = txs.iter().enumerate().map(|(i, tx)| (tx.id(), i)).collect();
    let mut receipts = BTreeMap::new();
    for (id, receipt) in run {
        let Some(&i) = at.get(&id) else { continue };
        let receipt = if failed.contains_key(&i) && receipt.outcome.is_ok() { receipt.into_failed(ReceiptOutcome::Native) } else { receipt };
        receipts.entry(i).or_insert(receipt);
    }
    apply_claims_budget(&mut receipts);
    receipts
}

/// `Chain::replay`'s, once the startup replay ran the logged `block`'s transactions again on
/// `state`: takes the receipts that run computed (so none stay behind in the state) and says
/// whether they're the ones the block log holds. The replay keeps the logged ones either way:
/// receipts are display only (§8.1), and P2's replay gate is what holds them to determinism
/// across binaries. A difference means this binary computes a receipt otherwise than the one
/// that logged the block, which is logged at error level, and `false` here (`Chain::replay` counts
/// those in `Chain::receipt_mismatches`). Nothing to do for a block without program transactions.
pub fn replayed(state: &mut State, block: &crate::types::Block) -> bool {
    let again = for_block(state, block.height, &block.txs, &block.failed);
    let same = again == block.programs;
    if !same {
        let txs: std::collections::BTreeSet<usize> = again.keys().chain(block.programs.keys()).copied().collect();
        let differ: Vec<usize> = txs.into_iter().filter(|i| again.get(i) != block.programs.get(i)).collect();
        tracing::error!(height = block.height, txs = ?differ, "the program receipts this binary computes for a logged block differ from the logged ones, which it keeps (display only: no state differs)");
    }
    same
}

/// The claims budget (§8.1): in the block's order, a receipt whose claims fit what's left of
/// [`MAX_BLOCK_CLAIM_BYTES`] records them and spends that much of it; one whose claims don't
/// records none of them, says what it left out (`omitted`), and spends nothing, so a later, smaller
/// one may still fit. Every node computes the same receipts in the same order, so the cut is the
/// same everywhere.
pub fn apply_claims_budget(receipts: &mut BTreeMap<usize, ProgramReceipt>) {
    let mut left = MAX_BLOCK_CLAIM_BYTES;
    for receipt in receipts.values_mut() {
        let claims = receipt.claim_bytes();
        match left.checked_sub(claims) {
            Some(rest) => left = rest,
            None => receipt.omit_claims(),
        }
    }
}

impl ProgramReceipt {
    /// What its claims count against the block's budget: each event's data, name and
    /// [`EVENT_CLAIM_BYTES`], and each call's return data.
    pub fn claim_bytes(&self) -> u64 {
        let events = self.events.iter().map(|e| (e.data.0.len() + e.name.len()) as u64 + EVENT_CLAIM_BYTES);
        let returns = self.calls.iter().map(|c| c.return_data.0.len() as u64);
        events.chain(returns).fold(0, u64::saturating_add)
    }

    /// Leaves its claims out, saying what they were.
    fn omit_claims(&mut self) {
        let event_bytes = self.events.iter().map(|e| (e.data.0.len() + e.name.len()) as u64).fold(0, u64::saturating_add);
        let return_bytes = self.calls.iter().map(|c| c.return_data.0.len() as u64).fold(0, u64::saturating_add);
        self.omitted = Some(Omitted { events: self.events.len() as u64, event_bytes, return_bytes });
        self.events.clear();
        for call in &mut self.calls {
            call.return_data = HexBytes::default();
        }
    }

    /// This receipt with `outcome`, a failure: what it paid and the fuel it used stay, and nothing
    /// of its action does (no return data, events, balance changes, storage, debits outside the VM,
    /// burn, program or blobs).
    fn into_failed(self, outcome: ReceiptOutcome) -> ProgramReceipt {
        let calls = self.calls.into_iter().map(|c| CallResult { return_data: HexBytes::default(), ..c }).collect();
        ProgramReceipt { outcome, calls, events: vec![], omitted: None, balance_changes: vec![], storage: StorageSummary::default(), outside_vm: OutsideVm::default(), burned_by_programs: 0, created: None, upload: None, ..self }
    }
}

/// A trap's kind in words: `OutOfFuel` → "out of fuel".
fn trap_words(kind: TrapKind) -> String {
    Numbered::of(kind.number(), kind).name.replace('_', " ")
}

fn commit_words(e: CommitError) -> &'static str {
    match e {
        CommitError::Overflow => "a sum overflowed",
        CommitError::NotConserved => "FANG not conserved",
        CommitError::Debited => "an account debited that may not be",
    }
}

impl Failure {
    /// The failed transaction's reason (`Outcome::Failed`'s text: display only, which no replay
    /// compares), naming programs by their addresses under the chain's `prefix`. It says what its
    /// receipt's outcome ([`ReceiptOutcome::failed`]) numbers, and for a native failure, which the
    /// receipt only numbers, how.
    pub fn reason(&self, prefix: &str) -> String {
        let program = |a: &Address| short(&crypto::render_address(prefix, a));
        match self {
            Failure::Vm(FailCode::Trapped { program: p, kind }) => format!("program {} trapped: {} (trap {})", program(p), trap_words(*kind), kind.number()),
            Failure::Vm(FailCode::Aborted { program: p, code, .. }) => format!("program {} aborted with code {code}", program(p)),
            Failure::Vm(FailCode::Commit(e)) => format!("the programs' changes failed the commit checks ({}, commit error {})", commit_words(*e), e.number()),
            Failure::ValueCap { cap, control } => format!("programs would control {control} drops (value held plus sponsored deposits), past the value cap of {cap}"),
            Failure::ExitOnly => "programs are exit-only: a call may not raise the FANG programs control".into(),
            Failure::StorageLimit { deposits, limit } => format!("deposits of {deposits} drops pass the transaction's storage limit of {limit}"),
            Failure::StateFull => "program state is full: no storage price past its ceiling".into(),
            Failure::InvalidCode(e) => format!("invalid code: upload validation rule {} ({:?})", e.rule.number(), e.rule),
            Failure::TranslationRefused => "the engine refused to translate the code".into(),
            Failure::InterfaceMismatch(m) => format!("the interface doesn't match the code: {m}"),
            Failure::Native(why) => why.clone(),
        }
    }
}

#[cfg(test)]
mod tests;
