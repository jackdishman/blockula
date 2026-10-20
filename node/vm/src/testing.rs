//! `MemHost`: an in-memory chain to run programs on (balances, programs, code, storage), for
//! tests, the guests' checks, fuzzing and calibration (feature `testing`: never in a node build).
//! Applying a receipt's diff is the test's choice, as commit would be the node's.
//!
//! **Code arrives only by upload** ([`MemHost::upload`], as `UploadCode` would: 4.3, 7.1): upload
//! validation first, then a test translation on a throwaway soroban-wasmi engine, and only then is
//! it stored. So every module a block engine ever loads passed [`validate`] before any engine saw
//! it: no engine's translator is trusted with code validation refused. A panic in the test
//! translation is caught as a backstop and refuses the upload; the throwaway engine is dropped
//! with it.

use crate::engine::{Address, BlockEngine, CodeHash, EngineKind, LoadError, new_block_engine};
use crate::exec::Diff;
use crate::host::View;
use crate::meter::{self, Schedule};
use crate::overlay::Entry;
use crate::validate::{ValidateError, validate};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// An address from a label: BLAKE3 of it, so tests read by name.
pub fn addr(label: &str) -> Address {
    *blake3::hash(label.as_bytes()).as_bytes()
}

/// `wasm` without its custom sections, as `blockula program build` strips them (upload validation
/// refuses them). For test modules from WAT, which writes a `name` section whenever the text names
/// anything. Panics on malformed input: a test's bug.
pub fn strip_custom_sections(wasm: &[u8]) -> Vec<u8> {
    let mut out = wasm[..8].to_vec();
    let mut at = 8;
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        if let Some((id, range)) = payload.expect("a well-formed test module").as_section() {
            if id != 0 {
                out.extend_from_slice(&wasm[at..range.end]);
            }
            at = range.end;
        }
    }
    out
}

/// Why an upload was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadError {
    /// Upload validation (4.2) refused it: no engine saw it.
    Invalid(ValidateError),
    /// It passed validation, but the test translation refused it.
    Translate { engine: EngineKind, error: LoadError },
    /// The engine panicked translating it (caught; the throwaway engine went with it).
    Panicked { engine: EngineKind },
}

impl fmt::Display for UploadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UploadError::Invalid(e) => write!(f, "invalid code: {e}"),
            UploadError::Translate { engine, error } => write!(f, "{}: {error}", engine.name()),
            UploadError::Panicked { engine } => write!(f, "{} panicked translating it", engine.name()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemHost {
    pub height: u64,
    pub balances: BTreeMap<Address, u64>,
    /// Programs and their code hash (each is also an account in `balances`).
    pub programs: BTreeMap<Address, CodeHash>,
    /// Uploaded code, by hash: private, so nothing reaches it but [`MemHost::upload`].
    codes: BTreeMap<CodeHash, Vec<u8>>,
    pub storage: BTreeMap<(Address, Vec<u8>), Entry>,
    /// Deposits held for storage entries.
    pub escrow: u128,
    /// Account-creation fees burned.
    pub burned: u128,
    pub storage_price: u64,
    pub creation_fee: u64,
}

impl Default for MemHost {
    fn default() -> Self {
        MemHost {
            height: 1,
            balances: BTreeMap::new(),
            programs: BTreeMap::new(),
            codes: BTreeMap::new(),
            storage: BTreeMap::new(),
            escrow: 0,
            burned: 0,
            storage_price: 100,
            creation_fee: 100_000_000,
        }
    }
}

impl MemHost {
    pub fn new() -> Self {
        MemHost::default()
    }

    /// Opens `account` with `balance` drops (or sets it).
    pub fn fund(&mut self, account: Address, balance: u64) -> &mut Self {
        self.balances.insert(account, balance);
        self
    }

    /// `UploadCode` (7.1): [`validate`], then a test translation on a throwaway soroban-wasmi
    /// engine (the node's engine; the `ref038` oracle never decides what uploads), then the code
    /// is stored under its hash. Nothing is stored if any step refuses it. Uploading the same code
    /// again is a no-op that returns its hash.
    pub fn upload(&mut self, wasm: &[u8]) -> Result<CodeHash, UploadError> {
        validate(wasm).map_err(UploadError::Invalid)?;
        let hash = *blake3::hash(wasm).as_bytes();
        let engine = EngineKind::Soroban;
        // The schedule doesn't reach translation; any will do for a throwaway engine.
        let loaded = catch_unwind(AssertUnwindSafe(|| new_block_engine(engine, &meter::V1).load(hash, wasm)));
        match loaded {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(UploadError::Translate { engine, error }),
            Err(_) => return Err(UploadError::Panicked { engine }),
        }
        self.codes.insert(hash, wasm.to_vec());
        Ok(hash)
    }

    /// Uploaded code, by hash.
    pub fn code(&self, hash: &CodeHash) -> Option<&[u8]> {
        self.codes.get(hash).map(Vec::as_slice)
    }

    /// Creates a program at `address` running uploaded `code` (an account with no balance yet).
    /// Panics if `code` wasn't uploaded: a test's bug.
    pub fn create(&mut self, address: Address, code: CodeHash) {
        assert!(self.codes.contains_key(&code), "create: code {} wasn't uploaded", hex(&code));
        self.programs.insert(address, code);
        self.balances.entry(address).or_insert(0);
    }

    /// [`upload`](Self::upload) and [`create`](Self::create): a program at `address` running
    /// `wasm`. Panics if the upload is refused: a test's bug.
    pub fn deploy(&mut self, address: Address, wasm: &[u8]) -> CodeHash {
        let hash = self.upload(wasm).unwrap_or_else(|e| panic!("upload refused: {e}"));
        self.create(address, hash);
        hash
    }

    /// A fresh block engine with every uploaded code loaded.
    pub fn engine(&self, kind: EngineKind, schedule: &Schedule) -> Result<Box<dyn BlockEngine>, LoadError> {
        let mut engine = new_block_engine(kind, schedule);
        for (hash, wasm) in &self.codes {
            engine.load(*hash, wasm)?;
        }
        Ok(engine)
    }

    /// A fresh block engine with exactly the code of `programs` loaded (each distinct code once),
    /// as a block loads its transactions' declared programs before running any (4.3). Accounts
    /// that aren't programs are skipped. Also returns the load fuel the block's admission would
    /// charge (`Schedule::load_fuel` per distinct code).
    pub fn engine_for(&self, kind: EngineKind, schedule: &Schedule, programs: &[Address]) -> Result<(Box<dyn BlockEngine>, u64), LoadError> {
        let codes: BTreeSet<CodeHash> = programs.iter().filter_map(|p| self.programs.get(p).copied()).collect();
        let mut engine = new_block_engine(kind, schedule);
        let mut fuel = 0u64;
        for hash in codes {
            let wasm = &self.codes[&hash];
            engine.load(hash, wasm)?;
            fuel = fuel.saturating_add(schedule.load_fuel(wasm.len()));
        }
        Ok((engine, fuel))
    }

    /// Σ balances + escrow + burned: what conservation keeps constant.
    pub fn total(&self) -> u128 {
        self.balances.values().map(|b| u128::from(*b)).sum::<u128>() + self.escrow + self.burned
    }

    /// Applies a successful receipt's diff, as commit would (4.4): storage, then balances, then
    /// opened accounts. Panics on a diff that doesn't fit the state, which is a test failure.
    pub fn apply(&mut self, diff: &Diff) {
        for change in &diff.storage {
            let key = (change.program, change.key.clone());
            match &change.entry {
                Some(entry) => self.storage.insert(key, entry.clone()),
                None => self.storage.remove(&key),
            };
        }
        self.escrow = u128::try_from(i128::try_from(self.escrow).unwrap() + diff.escrow).expect("escrow stays non-negative");
        for (account, delta) in &diff.balances {
            let balance = self.balances.entry(*account).or_insert(0);
            *balance = u64::try_from(i128::from(*balance) + delta).expect("balances stay in range");
        }
        for account in &diff.opened {
            self.balances.entry(*account).or_insert(0);
        }
        self.burned += diff.burned;
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl View for MemHost {
    fn height(&self) -> u64 {
        self.height
    }
    fn balance(&self, account: &Address) -> Option<u64> {
        self.balances.get(account).copied()
    }
    fn code_of(&self, program: &Address) -> Option<CodeHash> {
        self.programs.get(program).copied()
    }
    fn storage(&self, program: &Address, key: &[u8]) -> Option<Entry> {
        self.storage.get(&(*program, key.to_vec())).cloned()
    }
    fn storage_price(&self) -> u64 {
        self.storage_price
    }
    fn account_creation_fee(&self) -> u64 {
        self.creation_fee
    }
}
