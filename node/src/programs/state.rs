//! Programs' state (docs/PROGRAMS-V0.md §4): the format-1 records of namespaces 66–69 and 72 and
//! of META `programs`, the module's slice of `State` ([`Programs`]) and its undo log
//! ([`ProgramsJournal`]), what isn't a record ([`BlockRun`], [`Fault`]), and the consistency rules
//! `State::check_invariants` runs ([`check_invariants`], [`check_bounds`]).
//!
//! **Nothing here exists until programs first switch on** (`super::activate::on_activate`): every
//! map is empty and `meta` is `None` until then, so the state holds exactly v1's records under v1's
//! root (testnet-2's, and a v2 chain's while programs are dormant). Once created, META `programs`
//! is never deleted: switching programs off leaves it (exit-only, §1.4).
//!
//! **Formats.** Every value is a [`Versioned`] record: a format byte (1), then the Borsh body
//! (`state/codec.rs`'s conventions). A later protocol version adds format 2 beside it and reads
//! both forever, so no existing record is re-encoded and `SCHEMA_VERSION` stays 9 (§4.7, §9.1).
//! Field order is encoding order, frozen with v2's other program rules (§9.2).
//!
//! | Namespace | Key | Value |
//! |---|---|---|
//! | 66 `PROGRAMS` | the program's address, as a string (as accounts are keyed) | [`ProgramRecord`] |
//! | 67 `PROGRAM_CODE` | `wasm_hash` | [`CodeRecord`], written once |
//! | 68 `PROGRAM_INTERFACES` | `interface_hash` | [`InterfaceRecord`], written once |
//! | 69 `PROGRAM_STORAGE` | the program's 32 address bytes ‖ its key (1–128 bytes) ([`storage_key`]) | [`Entry`] |
//! | 72 `PROGRAM_BLOB_REFS` | kind ‖ hash ([`blob_key`]) | [`BlobRef`] |
//! | META `programs` | | [`ProgramsMeta`] |

use crate::chain::{Journal, State};
use crate::state::codec::{decode, encode, ByteKey};
use crate::state::{meta, ns, JMap, SubState, TMap, Table};
use crate::upgrade;
use anyhow::{bail, ensure, Result};
use blockula_vm::config::{MAX_CODE_BYTES, MAX_INTERFACE_BYTES, MAX_KEY_BYTES, MAX_METHODS, MAX_VALUE_BYTES};
use blockula_vm::interface::Interface;
use blockula_vm::storage::{capacity, CAPACITY_UNIT, MAX_CAPACITY};
use borsh::{BorshDeserialize, BorshSerialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::ops::{Deref, DerefMut};

/// The record format this binary writes, and the only one it reads.
pub const FORMAT: u8 = 1;

/// A program record's value: its format byte ([`FORMAT`]), then `T`. Decoding refuses any other
/// first byte, so this binary never misreads a record a later format wrote (and a later binary
/// tells the formats apart by it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Versioned<T>(pub T);

impl<T> Deref for Versioned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for Versioned<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T: BorshSerialize> BorshSerialize for Versioned<T> {
    fn serialize<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        FORMAT.serialize(w)?;
        self.0.serialize(w)
    }
}

impl<T: BorshDeserialize> BorshDeserialize for Versioned<T> {
    fn deserialize_reader<R: Read>(r: &mut R) -> std::io::Result<Self> {
        let format = u8::deserialize_reader(r)?;
        if format != FORMAT {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("program record format {format}, this binary reads {FORMAT}")));
        }
        Ok(Versioned(T::deserialize_reader(r)?))
    }
}

/// A program (namespace 66), format 1. Callable from the block after `created_at` (A5). No pending
/// upgrade, offered authority or status: the actions that need them (tags 65–67) bring them in
/// format 2 (A19), so every v0 program is active.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ProgramRecord {
    /// The account whose transaction created it.
    pub creator: String,
    pub created_at: u64,
    pub wasm_hash: [u8; 32],
    pub interface_hash: [u8; 32],
    /// 1; only an upgrade (tag 65) would raise it. It's in the program's `version_digest` (§2.1).
    pub version: u32,
    /// Stored as created, with no power until upgrades exist (A19).
    pub authority: Option<String>,
    /// 0 without an authority.
    pub upgrade_delay_blocks: u64,
    /// 0 open, 1 additive (inert until upgrades exist).
    pub policy: u8,
    /// Whether a plain `Transfer` may credit it (§7.1).
    pub accepts_transfers: bool,
    /// Its storage entries: how many, Σ their capacities, Σ their deposits.
    pub items: u64,
    pub bytes: u64,
    pub deposit: u64,
    /// Σ the deposits of its entries it paid itself (`SPONSOR`, §6.1), at most `deposit`: escrow
    /// the program controls, so part of the FANG programs control (§7.1, A40).
    pub sponsored: u64,
    /// What its creator bonded for the record itself ([`super::RECORD_BYTES`] at the price of the
    /// moment), and who: kept for the refund a later `CloseProgram` makes (A18).
    pub record_deposit: u64,
    pub record_payer: String,
}

/// A program's code (namespace 67), format 1. Written once at upload and never rewritten: its
/// [`BlobRef`] carries what changes.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CodeRecord {
    pub wasm: Vec<u8>,
    /// Its method names: the `bk_` function exports without the prefix, ascending (W1's
    /// `validate::ModuleInfo::exports`).
    pub exports: Vec<String>,
}

/// An interface (namespace 68), format 1 (§3), written once. In memory it also holds the decoded
/// [`Interface`], decoded when the record is made or loaded ([`InterfaceRecord::new`]), so
/// admission and execution never parse one (§4.2, §6.4). Only `bytes` is encoded: the record's
/// Borsh form is exactly `bytes`' (a u32 length, then the bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceRecord {
    pub bytes: Vec<u8>,
    /// `bytes` decoded (`blockula_vm::interface::decode`); `None` if they don't decode, which no
    /// upload stores (its interface decoded at the door, `Tx::check_format`), so admission refuses
    /// anything that names such an interface rather than trusting it.
    interface: Option<Interface>,
}

impl InterfaceRecord {
    /// The record of an interface's bytes, decoding them.
    pub fn new(bytes: Vec<u8>) -> InterfaceRecord {
        let interface = blockula_vm::interface::decode(&bytes).ok();
        InterfaceRecord { bytes, interface }
    }

    /// The decoded interface (`None` if the bytes don't decode).
    pub fn interface(&self) -> Option<&Interface> {
        self.interface.as_ref()
    }
}

impl BorshSerialize for InterfaceRecord {
    fn serialize<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        self.bytes.serialize(w)
    }
}

impl BorshDeserialize for InterfaceRecord {
    fn deserialize_reader<R: Read>(r: &mut R) -> std::io::Result<Self> {
        Ok(InterfaceRecord::new(Vec::<u8>::deserialize_reader(r)?))
    }
}

/// One entry of a program's storage (namespace 69), format 1: W1's `overlay::Entry` as stored.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Entry {
    pub value: Vec<u8>,
    /// Whose deposit it is (32 address bytes, the VM's own type), refunded to exactly that account:
    /// the signer's, or the program's own when it paid itself (`SPONSOR`).
    pub payer: [u8; 32],
    /// Fixed when it was bonded.
    pub deposit: u64,
    /// The accounted bytes the deposit covers: `storage::capacity` of the key and the value it was
    /// bonded for (an overwrite that fits keeps it, and bonds nothing).
    pub capacity: u32,
}

/// [`BlobRef`] key kinds: code, and interface.
pub const BLOB_CODE: u8 = 0;
pub const BLOB_INTERFACE: u8 = 1;

/// Who paid for a stored blob, and who uses it (namespace 72), format 1.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct BlobRef {
    /// Who bonded the deposit (for the refund a later `RemoveCode` makes, A18).
    pub uploader: String,
    pub deposit: u64,
    /// The blob's length.
    pub size: u32,
    /// How many program records cite it.
    pub refs: u32,
    /// Usable from the block after (A5).
    pub uploaded_at: u64,
    /// Moved by a re-upload of the same bytes.
    pub last_used_at: u64,
}

/// A run of pauses (§7.2): programs are paused while `from ≤ h < until`. A renewal moves `until`,
/// never past `started` + the cap; the record stays after it lapses, so the spacing between runs
/// can be checked without an end-of-block step.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Pause {
    pub from: u64,
    pub until: u64,
    /// Where the run began (its first pause's `from`).
    pub started: u64,
    /// The validators whose signatures started or last renewed it.
    pub signers: Vec<String>,
}

/// META `programs`, format 1: programs' scalars. Present once programs first switched on.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ProgramsMeta {
    /// The parameters (§4.5, `super::params`), a sorted map so a later version adds keys freely.
    pub params: BTreeMap<String, u64>,
    /// How many program records exist.
    pub programs: u64,
    /// Program state's accounted bytes: code + interfaces + entry capacities + `RECORD_BYTES` per
    /// program (what the storage price curve reads, §4.6).
    pub state_bytes: u64,
    /// Σ balances of program accounts (§7.1).
    pub value_held: u64,
    /// Σ records' `sponsored`: the escrow programs paid themselves (§7.1).
    pub sponsored: u64,
    /// Σ deposits: entries, blobs and program records. Held by this module (the supply counts it).
    pub escrow: u64,
    /// The fuel rate, in milli-drops per fuel, moved each block (§5.6).
    pub fuel_rate: u64,
    /// The last run of pauses, kept after it lapses (§7.2).
    pub pause: Option<Pause>,
    /// Validator → the height of its pending pause signature.
    pub votes: BTreeMap<String, u64>,
    /// Validator → the height of the last pause it started or renewed.
    pub pause_signed: BTreeMap<String, u64>,
}

/// The block being executed, as far as programs are concerned. Not a record (outside the root,
/// never in a snapshot, which is taken at a block boundary). Keyed by height: a reader judging
/// height `h` takes a `BlockRun` of another height as empty ([`BlockRun::fuel_at`]), and the first
/// write at `h` resets it ([`block_mut`], `receipt::record`), so it needs no start-of-block step
/// (§4.3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockRun {
    pub height: u64,
    /// Program fuel counted so far in the block at `height` (§5.4).
    pub fuel: u64,
    /// The receipts of the program transactions the block at `height` ran so far, applied or
    /// failed, in order, each with its transaction's id (§8.1). Not journaled: a failed
    /// transaction's receipt outlives its action's rollback, which is the point. One of a
    /// transaction rolled back whole (left out of a proposal, or skipped) may stay behind, which
    /// is harmless: `receipt::for_block` keeps only those of transactions the block includes. A
    /// rollback to an older height empties them ([`ProgramsJournal::rollback`]).
    pub receipts: Vec<(String, super::receipt::ProgramReceipt)>,
}

impl BlockRun {
    /// The fuel counted in the block at `height`: 0 while this run is another height's.
    pub fn fuel_at(&self, height: u64) -> u64 {
        if self.height == height { self.fuel } else { 0 }
    }
}

/// What a node fault was tied to (§6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultClass {
    /// The transaction: anything raised validating, translating, loading or running its code.
    Tx,
    /// This node couldn't make what every program transaction needs (a thread, an engine).
    Setup,
}

/// A node fault (§6.2): never a transaction outcome. Not a record, and never journaled: sticky for
/// the life of the `State` value that hit it, across heights and into its clones; only a
/// proposer's trial takes it (`Option::take`) to leave the transaction out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    pub height: u64,
    /// The transaction's id.
    pub tx: String,
    pub class: FaultClass,
    /// The fault's `Display`, for logs.
    pub what: String,
}

/// Programs' slice of the state (the table in the module docs).
#[derive(Clone, Debug, Default)]
pub struct Programs {
    pub records: TMap<String, Versioned<ProgramRecord>>,
    pub code: TMap<[u8; 32], Versioned<CodeRecord>>,
    pub interfaces: TMap<[u8; 32], Versioned<InterfaceRecord>>,
    pub storage: TMap<ByteKey, Versioned<Entry>>,
    pub blob_refs: TMap<ByteKey, Versioned<BlobRef>>,
    /// META `programs` while `Some`: programs have switched on at least once.
    pub meta: Option<ProgramsMeta>,
    /// Not a record (above).
    pub block: BlockRun,
    /// Not a record (above).
    pub fault: Option<Fault>,
    /// Test builds: which simulated node this state is the lineage of (`sim.rs`, which tags each
    /// node's chain once, and every clone carries the tag), so that a fault injected for some nodes
    /// only (`view::inject::arm_on`) meets only theirs, on whichever thread the program runs. Not a
    /// record; `None` everywhere else.
    #[cfg(test)]
    pub node: Option<usize>,
}

impl SubState for Programs {
    fn tables(&self) -> Vec<(u8, &dyn Table)> {
        let Programs { records, code, interfaces, storage, blob_refs, meta: _, block: _, fault: _, #[cfg(test)] node: _ } = self;
        vec![(ns::PROGRAMS, records), (ns::PROGRAM_CODE, code), (ns::PROGRAM_INTERFACES, interfaces), (ns::PROGRAM_STORAGE, storage), (ns::PROGRAM_BLOB_REFS, blob_refs)]
    }

    fn tables_mut(&mut self) -> Vec<(u8, &mut dyn Table)> {
        let Programs { records, code, interfaces, storage, blob_refs, meta: _, block: _, fault: _, #[cfg(test)] node: _ } = self;
        vec![(ns::PROGRAMS, records), (ns::PROGRAM_CODE, code), (ns::PROGRAM_INTERFACES, interfaces), (ns::PROGRAM_STORAGE, storage), (ns::PROGRAM_BLOB_REFS, blob_refs)]
    }

    /// META `programs`, only once programs have switched on: until then the state's scalars, and
    /// so its root, are v1's.
    fn meta_records(&self) -> Vec<(&'static [u8], Vec<u8>)> {
        self.meta.iter().map(|m| (meta::PROGRAMS, encode(&Versioned(m)))).collect()
    }

    fn load_meta(&mut self, key: &[u8], value: &[u8]) -> Result<bool> {
        if key != meta::PROGRAMS {
            return Ok(false);
        }
        let Versioned(m) = decode::<Versioned<ProgramsMeta>>(value)?;
        self.meta = Some(m);
        Ok(true)
    }
}

/// Undo log for [`Programs`] (part of `chain::Journal`): the old value of every touched record,
/// META as it was (`Some(None)`: it didn't exist yet, so a rolled-back first activation leaves
/// none), and the block run's `(height, fuel)` as it was (§4.3: a rolled-back trial restores both,
/// so a rolled-back first trial at a new height leaves the older height's run, which readers at the
/// new one take as empty). `opened` isn't an undo record (below).
#[derive(Default)]
pub struct ProgramsJournal {
    pub records: JMap<String, Versioned<ProgramRecord>>,
    pub code: JMap<[u8; 32], Versioned<CodeRecord>>,
    pub interfaces: JMap<[u8; 32], Versioned<InterfaceRecord>>,
    pub storage: JMap<ByteKey, Versioned<Entry>>,
    pub blob_refs: JMap<ByteKey, Versioned<BlobRef>>,
    pub meta: Option<Option<ProgramsMeta>>,
    pub block: Option<(u64, u64)>,
    /// Not an undo record, like `Journal::lock_slack`: the accounts a program's commit opened in
    /// this transaction, whose creation fees the commit burned already, so
    /// `econ::charge_new_accounts` doesn't bill the signer for them again (§6.1 step 6.3).
    pub opened: BTreeSet<String>,
}

impl ProgramsJournal {
    /// Folds a nested journal in, keeping the oldest values.
    pub fn absorb(&mut self, other: ProgramsJournal) {
        let ProgramsJournal { records, code, interfaces, storage, blob_refs, meta, block, opened } = other;
        self.records.absorb(records);
        self.code.absorb(code);
        self.interfaces.absorb(interfaces);
        self.storage.absorb(storage);
        self.blob_refs.absorb(blob_refs);
        if self.meta.is_none() {
            self.meta = meta;
        }
        if self.block.is_none() {
            self.block = block;
        }
        self.opened.extend(opened);
    }

    /// Puts everything recorded back as it was. The fault isn't journaled (§6.2).
    pub fn rollback(self, p: &mut Programs) {
        let ProgramsJournal { records, code, interfaces, storage, blob_refs, meta, block, opened: _ } = self;
        records.restore(&mut p.records);
        code.restore(&mut p.code);
        interfaces.restore(&mut p.interfaces);
        storage.restore(&mut p.storage);
        blob_refs.restore(&mut p.blob_refs);
        if let Some(m) = meta {
            p.meta = m;
        }
        if let Some((height, fuel)) = block {
            // Back to an older height: what this run collected was the newer height's.
            if p.block.height != height {
                p.block.receipts.clear();
            }
            p.block.height = height;
            p.block.fuel = fuel;
        }
    }

    /// Records META as it is, unless this journal already has.
    pub fn save_meta(&mut self, p: &Programs) {
        if self.meta.is_none() {
            self.meta = Some(p.meta.clone());
        }
    }
}

/// META `programs` for a change, journaled in `j`; `None` while programs have never switched on.
pub fn meta_mut<'a>(state: &'a mut State, j: &mut Journal) -> Option<&'a mut ProgramsMeta> {
    j.programs.save_meta(&state.programs);
    state.programs.meta.as_mut()
}

/// The block run of the block at `height`, for a change, journaled in `j`: its `(height, fuel)` as
/// they were go into the journal first, then a run left by another height is reset (the block's
/// first write).
pub fn block_mut<'a>(state: &'a mut State, j: &mut Journal, height: u64) -> &'a mut BlockRun {
    let b = &mut state.programs.block;
    if j.programs.block.is_none() {
        j.programs.block = Some((b.height, b.fuel));
    }
    if b.height != height {
        *b = BlockRun { height, ..BlockRun::default() };
    }
    b
}

/// What the program module holds of the supply: META's `escrow` (every deposit), 0 before programs
/// exist. Program accounts' balances are ordinary balances (`State::check_invariants` counts them).
pub fn escrow(state: &State) -> u128 {
    state.programs.meta.as_ref().map_or(0, |m| u128::from(m.escrow))
}

/// The key of a program's storage entry: its 32 address bytes ‖ its key.
pub fn storage_key(program: &[u8; 32], key: &[u8]) -> ByteKey {
    ByteKey([program.as_slice(), key].concat())
}

/// The key of a blob's [`BlobRef`]: kind ([`BLOB_CODE`], [`BLOB_INTERFACE`]) ‖ hash.
pub fn blob_key(kind: u8, hash: &[u8; 32]) -> ByteKey {
    ByteKey([[kind].as_slice(), hash].concat())
}

/// Programs' consistency rules (part of `State::check_invariants`): every line of
/// docs/PROGRAMS-V0.md §4.4's list, each quoted in the comment above its check. Every sum is
/// recomputed from the records: O(program state), and it validates each stored code once, so only
/// tests and the fuzzer run it (`State::debug_check`); production checks the totals alone at epoch
/// boundaries (`check_totals`, §7.1).
///
/// One line reads differently from §4.4's words: an entry's capacity *covers* its rounded size
/// (at least `storage::capacity` of its key and value, a whole number of units, at most the
/// largest), rather than equals it, because an overwrite that fits keeps the capacity it was bonded
/// for and bonds nothing (W1's overlay, §4.6).
pub fn check_invariants(state: &State) -> Result<()> {
    let p = &state.programs;
    let feature_on = upgrade::is_active(state, upgrade::PROGRAMS_FEATURE);
    // META absent ⇒ every program table is empty and the feature is off.
    let Some(m) = &p.meta else {
        ensure!(p.records.is_empty() && p.code.is_empty() && p.interfaces.is_empty() && p.storage.is_empty() && p.blob_refs.is_empty(), "program records exist without META programs");
        ensure!(!feature_on, "the programs feature is on without META programs");
        return Ok(());
    };
    // META present ⇒ the version the next block runs is ≥ 2 (the feature on ⇒ META present: above).
    let version = upgrade::version_at(state, crate::econ::now(state));
    ensure!(version >= upgrade::PROGRAMS_VERSION, "META programs exists under protocol v{version}");

    let prefix = state.params.prefix();
    // Program records by their 32 address bytes, which key their entries.
    let mut by_bytes: BTreeMap<[u8; 32], &str> = BTreeMap::new();
    for address in p.records.keys() {
        let bytes = crate::crypto::parse_address(address).filter(|(pf, _)| *pf == prefix).map(|(_, b)| b);
        let Some(bytes) = bytes else { bail!("program {address} isn't an address of this chain") };
        by_bytes.insert(bytes, address);
    }
    // Each program's entries, as the entries add up: (items, bytes, deposit, sponsored).
    let mut entries: BTreeMap<&str, (u64, u128, u128, u128)> = BTreeMap::new();
    let (mut entry_bytes, mut entry_deposits) = (0u128, 0u128);
    for (k, e) in p.storage.iter() {
        let key = &k.0;
        ensure!(key.len() > 32, "a program storage key of {} bytes has no key after the program's address", key.len());
        let program: [u8; 32] = key[..32].try_into().expect("32 bytes");
        // Each entry's program exists; each entry's capacity covers its rounded size; its value ≤ 16 KiB.
        let Some(&address) = by_bytes.get(&program) else { bail!("a storage entry of {} has no program", crate::crypto::render_address(prefix, &program)) };
        ensure!(e.value.len() <= MAX_VALUE_BYTES, "an entry of {address} holds {} bytes (max {MAX_VALUE_BYTES})", e.value.len());
        let need = capacity(key.len() - 32, e.value.len());
        ensure!(e.capacity >= need && e.capacity <= MAX_CAPACITY && (e.capacity as usize).is_multiple_of(CAPACITY_UNIT), "an entry of {address} has capacity {} for a rounded size of {need}", e.capacity);
        let t = entries.entry(address).or_default();
        t.0 += 1;
        t.1 += u128::from(e.capacity);
        t.2 += u128::from(e.deposit);
        if e.payer == program {
            t.3 += u128::from(e.deposit);
        }
        entry_bytes += u128::from(e.capacity);
        entry_deposits += u128::from(e.deposit);
    }

    // Each record's blobs exist; each `BlobRef.refs` is the number of records citing it; its size
    // is its blob's length; each `CodeRecord.exports` matches its code. Every blob has a `BlobRef`
    // and every `BlobRef` a blob (they're written together, and escrow counts the refs' deposits).
    let mut cited: BTreeMap<ByteKey, u64> = BTreeMap::new();
    for (address, r) in p.records.iter() {
        ensure!(p.code.contains_key(&r.wasm_hash) && p.interfaces.contains_key(&r.interface_hash), "program {address} cites a blob that isn't stored");
        *cited.entry(blob_key(BLOB_CODE, &r.wasm_hash)).or_default() += 1;
        *cited.entry(blob_key(BLOB_INTERFACE, &r.interface_hash)).or_default() += 1;
    }
    let mut blob_deposits = 0u128;
    for (k, b) in p.blob_refs.iter() {
        let hash: Option<[u8; 32]> = k.0.get(1..).and_then(|h| h.try_into().ok());
        let len = match (k.0.first(), hash) {
            (Some(&BLOB_CODE), Some(h)) => p.code.get(&h).map(|c| c.wasm.len()),
            (Some(&BLOB_INTERFACE), Some(h)) => p.interfaces.get(&h).map(|i| i.bytes.len()),
            _ => bail!("a blob reference is misfiled under {}", hex::encode(&k.0)),
        };
        ensure!(len.is_some_and(|l| l == b.size as usize), "blob reference {} doesn't match a stored blob of its size", hex::encode(&k.0));
        let refs = cited.get(k).copied().unwrap_or(0);
        ensure!(u64::from(b.refs) == refs, "blob reference {} counts {} programs, {refs} cite it", hex::encode(&k.0), b.refs);
        blob_deposits += u128::from(b.deposit);
    }
    for (hash, c) in p.code.iter() {
        ensure!(p.blob_refs.contains_key(&blob_key(BLOB_CODE, hash)), "code {} has no blob reference", hex::encode(hash));
        let exports = blockula_vm::validate::validate(&c.wasm).map(|info| info.exports);
        ensure!(exports.as_ref().is_ok_and(|e| *e == c.exports), "code {}'s exports aren't its code's", hex::encode(hash));
    }
    for hash in p.interfaces.keys() {
        ensure!(p.blob_refs.contains_key(&blob_key(BLOB_INTERFACE, hash)), "interface {} has no blob reference", hex::encode(hash));
    }

    // Each program's `items`, `bytes` and `deposit` match its entries, and its `sponsored` is Σ the
    // deposits of the entries it paid itself (so ≤ its `deposit`). Program accounts have no keys,
    // no locks and nonce 0, and every record has an account.
    let (mut held, mut sponsored, mut record_deposits) = (0u128, 0u128, 0u128);
    for (address, r) in p.records.iter() {
        let (items, bytes, deposit, own) = entries.get(address.as_str()).copied().unwrap_or_default();
        ensure!((u128::from(r.items), u128::from(r.bytes), u128::from(r.deposit)) == (u128::from(items), bytes, deposit), "program {address}'s storage totals drifted from its entries");
        ensure!(u128::from(r.sponsored) == own, "program {address}'s sponsored is {}, its own entries' deposits {own}", r.sponsored);
        let Some(acct) = state.accounts.get(address) else { bail!("program {address} has no account") };
        ensure!(acct.keys.is_empty() && acct.locks.is_empty() && acct.nonce == 0, "program {address}'s account has keys, locks or a nonce");
        held += u128::from(acct.balance);
        sponsored += u128::from(r.sponsored);
        record_deposits += u128::from(r.record_deposit);
    }
    // `value_held` = Σ balances of program accounts; `programs` = the number of records;
    // `sponsored` = Σ records' `sponsored`.
    ensure!(u128::from(m.value_held) == held, "value_held is {}, program accounts hold {held}", m.value_held);
    ensure!(m.programs == p.records.len() as u64, "META counts {} programs, {} records exist", m.programs, p.records.len());
    ensure!(u128::from(m.sponsored) == sponsored, "META's sponsored is {}, the records' add up to {sponsored}", m.sponsored);
    // `escrow` = Σ entry deposits + Σ blob deposits + Σ record deposits.
    let escrow = entry_deposits + blob_deposits + record_deposits;
    ensure!(u128::from(m.escrow) == escrow, "programs' escrow is {}, their deposits add up to {escrow}", m.escrow);
    // `state_bytes` = Σ code sizes + Σ interface sizes + Σ entry capacities + RECORD_BYTES × records,
    // and ≤ 2 × MAX_STATE_SOFT_BYTES (the bound: a lower parameter stops growth, it doesn't make the
    // state invalid).
    let code_bytes: u128 = p.code.values().map(|c| c.wasm.len() as u128).sum();
    let interface_bytes: u128 = p.interfaces.values().map(|i| i.bytes.len() as u128).sum();
    let bytes = code_bytes + interface_bytes + entry_bytes + u128::from(super::RECORD_BYTES) * p.records.len() as u128;
    ensure!(m.state_bytes <= 2 * super::params::MAX_STATE_SOFT_BYTES, "program state holds {} bytes, past twice the soft cap's bound", m.state_bytes);
    ensure!(u128::from(m.state_bytes) == bytes, "state_bytes is {}, program state holds {bytes}", m.state_bytes);

    // `params` has only known keys, each within its bounds; `program_max_tx_fuel ≤ program_max_block_fuel`.
    super::params::check_map(&m.params, &state.params)?;
    // `fuel_rate` in [1, MAX_FUEL_RATE] (the bound, not the floor parameter, which moves under it).
    ensure!((1..=super::MAX_FUEL_RATE).contains(&m.fuel_rate), "the fuel rate {} is outside [1, {}]", m.fuel_rate, super::MAX_FUEL_RATE);
    // `pause`: `from < until` (and a run starts where its first pause did, signed by validators);
    // `votes` and `pause_signed` name validators, at heights at or below the one the state's next
    // transaction runs at.
    let now = crate::econ::now(state);
    if let Some(pause) = &m.pause {
        ensure!(pause.started <= pause.from && pause.from < pause.until, "the programs pause runs from {} to {} (started {})", pause.from, pause.until, pause.started);
        let signers: BTreeSet<&String> = pause.signers.iter().collect();
        ensure!(!signers.is_empty() && signers.len() == pause.signers.len() && signers.iter().all(|v| state.validators.contains_key(*v)), "the programs pause's signers aren't distinct validators");
    }
    for (what, map) in [("vote", &m.votes), ("signature", &m.pause_signed)] {
        for (v, at) in map {
            ensure!(state.validators.contains_key(v) && *at <= now, "a programs pause {what} by {v} at block {at} (a validator, at block {now} or before)");
        }
    }
    Ok(())
}

/// Every program record within the caps the state machine enforces, so each fits what a snapshot
/// may carry (`state::MAX_KEY_BYTES`, `MAX_VALUE_BYTES`; part of `State::check_bounds`): code at
/// most 256 KiB with at most 64 method names, interfaces at most 16 KiB, storage keys of 1–128
/// bytes after the program's 32 and values of at most 16 KiB, blob references under 33-byte keys.
pub fn check_bounds(state: &State) -> Result<()> {
    let p = &state.programs;
    for (hash, c) in p.code.iter() {
        ensure!(c.wasm.len() <= MAX_CODE_BYTES && c.exports.len() <= MAX_METHODS && c.exports.iter().all(|e| blockula_vm::abi::is_ident(e)), "code {} is {} bytes with {} methods", hex::encode(hash), c.wasm.len(), c.exports.len());
    }
    for (hash, i) in p.interfaces.iter() {
        ensure!(i.bytes.len() <= MAX_INTERFACE_BYTES, "interface {} is {} bytes (max {MAX_INTERFACE_BYTES})", hex::encode(hash), i.bytes.len());
    }
    for (k, e) in p.storage.iter() {
        ensure!((33..=32 + MAX_KEY_BYTES).contains(&k.0.len()) && e.value.len() <= MAX_VALUE_BYTES, "a program storage entry has a {}-byte key and a {}-byte value", k.0.len(), e.value.len());
    }
    for k in p.blob_refs.keys() {
        ensure!(k.0.len() == 33, "a blob reference key is {} bytes", k.0.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
