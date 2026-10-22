//! `MemHost`: an in-memory chain to run programs on (balances, programs, code, storage), for
//! tests, the guests' checks, fuzzing and calibration (feature `testing`: never in a node build).
//! Applying a receipt's diff is the test's choice, as commit would be the node's.
//!
//! **Code arrives only by upload** ([`MemHost::upload`], as `UploadCode` would: 4.3, 7.1):
//! [`upload::check`] (upload validation, then a test translation on a throwaway soroban-wasmi
//! engine), and only then is it stored. So every module an engine ever loads passed validation
//! before any engine saw it. As on a node, a panic in either step is a node fault, never a refused
//! upload.

use crate::engine::{Address, BlockEngine, CodeHash, Digest, EngineKind, NodeFault, new_block_engine};
use crate::exec::{ANY_VERSION, Binding, Call, Diff};
use crate::host::{MethodInfo, MethodKind, View};
use crate::meter::{SCHEDULE_V0, Schedule};
use crate::overlay::Entry;
use crate::storage;
use crate::upload::{self, Refusal};
use std::collections::{BTreeMap, BTreeSet};

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

/// A program as `MemHost` holds it: its code, its version (1 at creation, +1 per
/// [`MemHost::upgrade`]) and its interface's methods.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemProgram {
    pub version: u32,
    /// Every method, by name. [`MemHost::create`] makes each of the code's methods a payable
    /// `Call`, so a guest's own checks (on value, say) run; [`MemHost::set_method`] changes one.
    pub methods: BTreeMap<String, MethodInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemHost {
    pub height: u64,
    pub balances: BTreeMap<Address, u64>,
    /// Programs and their code hash (each is also an account in `balances`).
    pub programs: BTreeMap<Address, CodeHash>,
    /// Each program's version and interface.
    pub interfaces: BTreeMap<Address, MemProgram>,
    /// Uploaded code, by hash, with its method names: private, so nothing reaches it but
    /// [`MemHost::upload`].
    codes: BTreeMap<CodeHash, (Vec<u8>, Vec<String>)>,
    pub storage: BTreeMap<(Address, Vec<u8>), Entry>,
    /// Deposits held for storage entries.
    pub escrow: u128,
    /// Account-creation fees burned.
    pub burned: u128,
    /// Accounted bytes of program state: here, the entries' capacities and nothing else.
    pub state_bytes: u64,
    /// Each program's storage totals (items, bytes, deposit), kept from diffs as the node's
    /// records are, and checked against the entries on every [`MemHost::apply`].
    pub totals: BTreeMap<Address, (u64, u64, u64)>,
    /// `program_storage_price` and `program_state_soft_bytes` (6.3's curve, `storage::price`).
    pub storage_price: u64,
    pub state_soft_bytes: u64,
    pub creation_fee: u64,
}

impl Default for MemHost {
    fn default() -> Self {
        MemHost {
            height: 1,
            balances: BTreeMap::new(),
            programs: BTreeMap::new(),
            interfaces: BTreeMap::new(),
            codes: BTreeMap::new(),
            storage: BTreeMap::new(),
            escrow: 0,
            burned: 0,
            state_bytes: 0,
            totals: BTreeMap::new(),
            storage_price: 100,
            state_soft_bytes: 512 << 20,
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

    /// `UploadCode` (7.1): [`upload::check`] (validation, then a test translation on a throwaway
    /// soroban-wasmi engine), then the code is stored under its hash. Shaped like
    /// `exec::execute`'s result: the inner `Err` is a refusal (the upload's failure; nothing is
    /// stored), the outer one a node fault (a panic in either step), never a refusal. Uploading the
    /// same code again is a no-op that returns its hash.
    pub fn upload(&mut self, wasm: &[u8]) -> Result<Result<CodeHash, Refusal>, NodeFault> {
        let info = match upload::check(wasm, &SCHEDULE_V0)? {
            Ok(info) => info,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let hash = *blake3::hash(wasm).as_bytes();
        self.codes.insert(hash, (wasm.to_vec(), info.exports));
        Ok(Ok(hash))
    }

    /// Uploaded code, by hash.
    pub fn code(&self, hash: &CodeHash) -> Option<&[u8]> {
        self.codes.get(hash).map(|(wasm, _)| wasm.as_slice())
    }

    /// Creates a program at `address` running uploaded `code` (an account with no balance yet), at
    /// version 1, every method a payable `Call`. Panics if `code` wasn't uploaded: a test's bug.
    pub fn create(&mut self, address: Address, code: CodeHash) {
        self.programs.insert(address, code);
        self.interfaces.insert(address, MemProgram { version: 1, methods: self.methods_of(&code) });
        self.balances.entry(address).or_insert(0);
    }

    /// Replaces a program's code (an upgrade taking effect, 7.3): its version goes up by one and
    /// its methods are the new code's, each a payable `Call`. Panics if either is missing.
    pub fn upgrade(&mut self, address: Address, code: CodeHash) {
        let methods = self.methods_of(&code);
        let program = self.interfaces.get_mut(&address).expect("upgrade: not a program");
        program.version += 1;
        program.methods = methods;
        self.programs.insert(address, code);
    }

    /// Sets one method's kind and payability, as an interface would declare them.
    pub fn set_method(&mut self, program: Address, name: &str, kind: MethodKind, payable: bool) -> &mut Self {
        let methods = &mut self.interfaces.get_mut(&program).expect("set_method: not a program").methods;
        assert!(methods.contains_key(name), "set_method: no method {name}");
        methods.insert(name.into(), MethodInfo { kind, payable });
        self
    }

    fn methods_of(&self, code: &CodeHash) -> BTreeMap<String, MethodInfo> {
        let (_, exports) = self.codes.get(code).unwrap_or_else(|| panic!("code {} wasn't uploaded", hex(code)));
        exports.iter().map(|m| (m.clone(), MethodInfo { kind: MethodKind::Call, payable: true })).collect()
    }

    /// `call` with the version digest it binds set to `program`'s in force (zeros if it isn't a
    /// program): what a wallet signs after previewing it.
    pub fn bind(&self, mut call: Call) -> Call {
        call.version_digest = self.version_digest(&call.program).unwrap_or(ANY_VERSION);
        call
    }

    /// Bindings to `programs` at any version.
    pub fn any_version(programs: &[Address]) -> Vec<Binding> {
        programs.iter().map(|&program| Binding { program, version_digest: ANY_VERSION }).collect()
    }

    /// [`upload`](Self::upload) and [`create`](Self::create): a program at `address` running
    /// `wasm`. Panics if the upload is refused or faults: a test's bug.
    pub fn deploy(&mut self, address: Address, wasm: &[u8]) -> CodeHash {
        let hash = self.upload(wasm).unwrap_or_else(|f| panic!("a node fault uploading: {f}")).unwrap_or_else(|e| panic!("upload refused: {e}"));
        self.create(address, hash);
        hash
    }

    /// A fresh engine with every uploaded code loaded.
    pub fn engine(&self, kind: EngineKind, schedule: &Schedule) -> Result<Box<dyn BlockEngine>, NodeFault> {
        let mut engine = new_block_engine(kind, schedule)?;
        for (hash, (wasm, _)) in &self.codes {
            engine.load(*hash, wasm).map_err(NodeFault::Load)?;
        }
        Ok(engine)
    }

    /// A fresh engine with exactly the code of `programs` loaded (each distinct code once), as the
    /// node loads a transaction's declared programs before running any (or, under 4.3, a block's).
    /// Accounts that aren't programs are skipped. Also returns the load fuel admission would
    /// charge (`Schedule::load_fuel` per distinct code).
    pub fn engine_for(&self, kind: EngineKind, schedule: &Schedule, programs: &[Address]) -> Result<(Box<dyn BlockEngine>, u64), NodeFault> {
        let codes: BTreeSet<CodeHash> = programs.iter().filter_map(|p| self.programs.get(p).copied()).collect();
        let mut engine = new_block_engine(kind, schedule)?;
        let mut fuel = 0u64;
        for hash in codes {
            let (wasm, _) = &self.codes[&hash];
            engine.load(hash, wasm).map_err(NodeFault::Load)?;
            fuel = fuel.saturating_add(schedule.load_fuel(wasm.len()));
        }
        Ok((engine, fuel))
    }

    /// Σ balances + escrow + burned: what conservation keeps constant.
    pub fn total(&self) -> u128 {
        self.balances.values().map(|b| u128::from(*b)).sum::<u128>() + self.escrow + self.burned
    }

    /// Applies a successful receipt's diff, as commit would (4.4): storage with its totals, then
    /// balances, then opened accounts. Panics on a diff that doesn't fit the state, or whose
    /// totals (`Diff::programs`, `state_bytes`, `escrow`) disagree with its entries: a test failure.
    pub fn apply(&mut self, diff: &Diff) {
        for change in &diff.storage {
            let key = (change.program, change.key.clone());
            match &change.entry {
                Some(entry) => self.storage.insert(key, entry.clone()),
                None => self.storage.remove(&key),
            };
        }
        let add = |total: u64, delta: i128| u64::try_from(i128::from(total) + delta).expect("a total stays in range");
        for p in &diff.programs {
            let (items, bytes, deposit) = self.totals.get(&p.program).copied().unwrap_or_default();
            let totals = (add(items, p.items.into()), add(bytes, p.bytes.into()), add(deposit, p.deposit));
            self.totals.insert(p.program, totals);
        }
        self.totals.retain(|_, t| *t != (0, 0, 0));
        self.state_bytes = add(self.state_bytes, diff.state_bytes.into());
        let mut recomputed: BTreeMap<Address, (u64, u64, u64)> = BTreeMap::new();
        for ((program, _), entry) in &self.storage {
            let t = recomputed.entry(*program).or_default();
            *t = (t.0 + 1, t.1 + u64::from(entry.capacity), t.2 + entry.deposit);
        }
        assert_eq!(self.totals, recomputed, "per-program totals from diffs disagree with the entries");
        assert_eq!(self.state_bytes, recomputed.values().map(|t| t.1).sum::<u64>(), "state bytes disagree with the entries");
        self.escrow = u128::try_from(i128::try_from(self.escrow).unwrap() + diff.escrow).expect("escrow stays non-negative");
        assert_eq!(self.escrow, recomputed.values().map(|t| u128::from(t.2)).sum::<u128>(), "escrow disagrees with the entries");
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
    /// A stand-in for 11.2's digest over the address, the code and the version (`MemHost` has no
    /// interface hash), under its own context so it's never mistaken for the node's.
    fn version_digest(&self, program: &Address) -> Option<Digest> {
        let code = self.programs.get(program)?;
        let version = self.interfaces.get(program)?.version;
        let mut h = blake3::Hasher::new_derive_key("blockula-vm testing version digest");
        h.update(program).update(code).update(&version.to_le_bytes());
        Some(*h.finalize().as_bytes())
    }
    fn method(&self, program: &Address, name: &str) -> Option<MethodInfo> {
        self.interfaces.get(program)?.methods.get(name).copied()
    }
    fn storage(&self, program: &Address, key: &[u8]) -> Option<Entry> {
        self.storage.get(&(*program, key.to_vec())).cloned()
    }
    fn state_bytes(&self) -> u64 {
        self.state_bytes
    }
    fn storage_price(&self, used: u64) -> Option<u64> {
        storage::price(self.storage_price, self.state_soft_bytes, used)
    }
    fn account_creation_fee(&self) -> u64 {
        self.creation_fee
    }
}
