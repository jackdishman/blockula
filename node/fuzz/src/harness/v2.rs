//! The `state_machine_programs` target's fixture and its program ops (docs/PROGRAMS-V0.md §10.4).
//!
//! **The fixture** ([`fixture`]) is the state machine's chain (`sm_build`: one validator that is
//! also the admin, six funded users, three agent keys, 4-block epochs) born at protocol v2 with the
//! `programs` feature on, and the four guests of `node/vm/tests/programs` (counter, token, escrow,
//! router) on it: one block uploads them, the next creates them with their interfaces (§2.1: a blob
//! is usable from the block after its upload, a program from the block after its creation), the
//! token with its `init`. The state an input starts from is the one after those two blocks
//! (height 2). Four **profiles** differ only in that state's parameters, and an input chooses one
//! with its first byte (modulo 4: a mutated byte is merely another profile, [`Profile::of`]):
//!
//! | Byte | Profile | What it adds |
//! |---|---|---|
//! | 0 | [`Profile::Default`] | nothing: programs' parameters at their defaults |
//! | 1 | [`Profile::Tight`] | the value cap at 500 FANG and the block's fuel at its floor (20 million), which a genesis can't set (programs' parameters are META's), so the cap and the block budget bind within one input's reach |
//! | 2 | [`Profile::ExitOnly`] | the feature switched off after the guests exist: programs are exit-only, where only calls that send no FANG run and nothing may be paid into a program |
//! | 3 | [`Profile::Pause`] | blocks of 2.4 hours (a day is 10 blocks, so a pause's day-long spans, its 30-day window and cap fall within a few hundred blocks) and four genesis validators (so a pause needs two signatures, `f + 1`) |
//!
//! **The checks** are the state machine's (`Sm::end_block`: the dry run's rollback, the incremental
//! root against a full rebuild, `check_invariants`, the record sizes, the roots' determinism and the
//! snapshot's round trip), and for programs, every block: no node fault (a program transaction that
//! faults the node is never a valid outcome, §6.2; a fault of the machine itself is a resource
//! error, not a finding), the totals check (`programs::check_totals`), the training wheels (no block
//! raises the FANG programs control past the value cap, §7.1, nor at all while programs are
//! exit-only, §7.3), and the oracles of `oracle.rs`, which hold each program transaction's charge,
//! fuel, balances, receipt and the guests' storage to the spec and to the state's own deltas, and
//! the block's counted fuel and the rate it leaves to §5.6. A quarter of inputs run twice, the
//! second time through `State::run_block`: the roots and the digests of each block's receipts must
//! be the unrolled block's.
//!
//! **The ops** are numbered after v1's (40–46; a kind byte is taken modulo [`OPS`] here), and each
//! is five bytes (`kind`, `a`, `b`, and the amount's two bytes) and a length byte with that many
//! bytes of payload. The op reads its fields from a stream, in order, its `b` and amount bytes
//! first and the payload after, every read past the end zero (so a short op is a plain one), and
//! `a` is its sender (`a % 6` of the users) and its tip. A **call** (40) reads one call
//! ([`Sm::call`]: the program, flags, the method, the value, the input), then the fuel, the storage
//! limit, the bindings and the fee ([`Fee`]); a **batch** (41) a count of calls (1–8) first. A
//! **create** (42) reads the blobs it names, flags (an authority, the initializer left out or
//! invented, a policy, whether it accepts transfers), then as a call does. An **upload** (43) is a
//! guest's code and interface with the fuzzer's edits to either. A **pause** (44) is a validator's
//! `ProgramsPause`, signed by a genesis validator, by a key a user registered a validator with, or
//! by a user for its own. A **pay** (45) is a transfer into a program, or into the address a create
//! will make (which a create adopts, §2.3). A **governance** step (46) does at a block boundary what
//! the governance executor does when a proposal passes: switch the feature off (exit-only) or on
//! again, or set a program parameter.

use super::oracle;
use super::*;
use crate::chain::BlockInfo;
use crate::programs::state::FaultClass;
use crate::programs::{self, digest, params, receipt::ProgramReceipt, state::ProgramRecord};
use crate::types::{Binding, HexBytes, InitCall, ProgramCall, ProgramPolicy};
use blockula_vm::config::{MAX_BINDINGS, MAX_CALLS};
use blockula_vm::host::MethodKind;
use blockula_vm::interface::{Method, Type};
use blockula_vm::meter::MAX_TX_FUEL;
use std::collections::BTreeMap;

pub(super) const OP_CALL: u8 = 40;
pub(super) const OP_BATCH: u8 = 41;
pub(super) const OP_CREATE: u8 = 42;
pub(super) const OP_UPLOAD: u8 = 43;
pub(super) const OP_PAUSE: u8 = 44;
pub(super) const OP_PAY: u8 = 45;
pub(super) const OP_GOV: u8 = 46;
/// A kind byte on this fixture is taken modulo this (v1's modulus is 40): the program ops are 40 to
/// this, exclusive.
pub(super) const OPS: u8 = 47;

const FANG: u64 = types::DROPS_PER_FANG;
/// The most a program transaction signs to pay when it signs "far above its charge": any chain the
/// fuzzer builds charges far less (`Tx::fee` is a maximum; only the including block's base fee and
/// fuel fee and the tip are charged).
pub(super) const PROGRAM_FEE: u64 = 5 * FANG;
/// The tight profile's value cap.
const TIGHT_VALUE_CAP: u64 = 500 * FANG;
/// The pause profile's block time: 2.4 hours, so a day is 10 blocks: the slowest chain whose
/// parameters' day-derived bounds are still ranges (`max_tx_validity_blocks` is `[10, day]`). A
/// pause's vote lasts 10 blocks, the pause 30, its sign window and its cap 300.
const PAUSE_BLOCK_MS: u64 = 8_640_000;
/// The pause profile's genesis validators (the first is the fixture's admin and proposer).
const PAUSE_VALIDATORS: usize = 4;
/// An input stops signing for program fuel once its transactions have asked for this much (the sum
/// of their fuel limits and uploads' fuel). An input's work is bounded by what it signed for: at
/// the 11 ns per fuel the floor runners measured natively, 40 million is under half a second; a
/// build with AddressSanitizer and coverage in the engine's loops is some tens of times slower, and
/// a run and its determinism rerun still end well inside the campaign's 10-second timeout. A
/// legal input that runs long is not a finding.
pub(super) const FUEL_CAP: u64 = 40_000_000;
/// The most blocks an input's run has: past it no more are made (an input of slow blocks, a few
/// hundred empty ones for a pause to lapse, is no slower than that). The pause profile's longest
/// span is 600 blocks, the spacing between two runs.
pub(super) const BLOCK_CAP: u64 = 800;
/// Empty blocks past this many, in a run, are made without the per-block checks (`Sm::quick_block`),
/// which cost far more than the block: 13 ms of the 800 would be 10 seconds. The blocks with
/// transactions in them are always checked in full.
pub(super) const FULL_EMPTY_BLOCKS: u32 = 8;

/// Which of the fixture's four states an input starts from (the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Profile {
    Default,
    Tight,
    ExitOnly,
    Pause,
}

impl Profile {
    pub(super) const ALL: [Profile; 4] = [Profile::Default, Profile::Tight, Profile::ExitOnly, Profile::Pause];

    /// The profile a first byte asks for: its place among [`Profile::ALL`], modulo their number.
    pub(super) fn of(byte: u8) -> Profile {
        Profile::ALL[byte as usize % Profile::ALL.len()]
    }

    /// The first byte that asks for this profile.
    pub(super) fn byte(self) -> u8 {
        Profile::ALL.iter().position(|p| *p == self).expect("a profile") as u8
    }

    /// The letter the docs and the reports call it by.
    pub(super) fn letter(self) -> char {
        match self {
            Profile::Default => 'G',
            Profile::Tight => 'T',
            Profile::ExitOnly => 'X',
            Profile::Pause => 'P',
        }
    }
}

/// One of the four guests (`node/vm/tests/programs`): its code and its interface.
pub(super) struct Guest {
    pub name: &'static str,
    pub wasm: &'static [u8],
    pub iface: &'static [u8],
}

macro_rules! guest {
    ($name:literal) => {
        Guest { name: $name, wasm: include_bytes!(concat!("../../../vm/tests/programs/", $name, ".wasm")), iface: include_bytes!(concat!("../../../vm/tests/programs/", $name, ".iface")) }
    };
}

/// The guests, in the order the fixture creates them: a program's index is its place here.
pub(super) const GUESTS: [Guest; 4] = [guest!("counter"), guest!("token"), guest!("escrow"), guest!("router")];

/// The programs of the fixture: its guests' addresses, in [`GUESTS`]' order.
pub(super) struct Guests {
    pub addresses: [String; 4],
}

/// The fixture of `profile` (built once per process).
pub(super) fn fixture(profile: Profile) -> &'static SmFixture {
    static F: [OnceLock<SmFixture>; 4] = [const { OnceLock::new() }; 4];
    F[profile as usize].get_or_init(|| build(profile))
}

/// Uploads the guests in a block and creates them in the next, on a chain born at v2 with programs
/// on; then `profile`'s parameters.
fn build(profile: Profile) -> SmFixture {
    let mut fx = match profile {
        Profile::Pause => sm_build(SM_CHAIN_V2, Build::V2 { validators: PAUSE_VALIDATORS, block_time_ms: PAUSE_BLOCK_MS }),
        _ => sm_build(SM_CHAIN_V2, Build::V2 { validators: 1, block_time_ms: 1_000 }),
    };
    if profile == Profile::Tight {
        let meta = fx.state.programs.meta.as_mut().expect("the genesis lists programs, so META `programs` exists");
        meta.params.insert(params::Param::ValueCap.key().to_string(), TIGHT_VALUE_CAP);
        meta.params.insert(params::Param::MaxBlockFuel.key().to_string(), params::MIN_BLOCK_FUEL);
        params::check_map(&meta.params, &fx.state.params).expect("the tight profile's parameters are within their bounds");
    }
    let (uploader, creator) = (&fx.users[1], &fx.users[0]);
    let mut sm = Sm::new(&fx);
    for g in &GUESTS {
        let upload = Action::UploadCode { wasm: HexBytes(g.wasm.to_vec()), interface: HexBytes(g.iface.to_vec()), storage_limit: 5 * FANG };
        sm.program_tx(uploader, &uploader.address, 0, Fee::Max, upload);
    }
    sm.end_block();
    // The token's `init` names its owner (the first user) and a supply of a million.
    let owner = crate::crypto::address_bytes(&creator.address).expect("a user's address parses");
    let supply = [owner.as_slice(), &1_000_000u64.to_le_bytes()].concat();
    let accepts = [true, false, true, false];
    let mut addresses = vec![];
    for (i, g) in GUESTS.iter().enumerate() {
        let init = (g.name == "token").then(|| InitCall { method: "init".into(), input: HexBytes(supply.clone()), value: 0 });
        let (fuel_limit, bindings) = if init.is_some() { (1_000_000, vec![]) } else { (0, vec![]) };
        let create = Action::CreateProgram {
            wasm_hash: hex::encode(digest::wasm_hash(g.wasm)),
            interface_hash: hex::encode(digest::interface_hash(g.iface)),
            authority: None,
            upgrade_delay_blocks: 0,
            policy: ProgramPolicy::Open,
            accepts_transfers: accepts[i],
            init,
            fuel_limit,
            storage_limit: 5 * FANG,
            bindings,
        };
        // Creates from one sender take its next nonces, which its addresses derive from (§2.1).
        addresses.push(digest::program_address(fx.state.params.prefix(), &owner, i as u64));
        sm.program_tx(creator, &creator.address, 0, Fee::Max, create);
    }
    sm.end_block();
    assert_eq!(sm.applied, 2 * GUESTS.len(), "the fixture's uploads and creates must all apply");
    let (mut state, height) = (sm.state, sm.height);
    for (g, address) in GUESTS.iter().zip(&addresses) {
        let record = state.programs.records.get(address).unwrap_or_else(|| panic!("the fixture's {} isn't at {address}", g.name));
        assert_eq!(record.wasm_hash, digest::wasm_hash(g.wasm), "the fixture's {} runs other code", g.name);
    }
    if profile == Profile::ExitOnly {
        let mut j = crate::chain::Journal::default();
        crate::upgrade::exec_deactivate_feature(&mut state, &mut j, crate::upgrade::PROGRAMS_FEATURE).expect("programs are on");
    }
    fx.state = state;
    fx.height = height;
    fx.programs = Some(Guests { addresses: addresses.try_into().expect("one address a guest") });
    fx
}

// ------------------------------------------------------------------------------------- the ops

/// Fuzzer bytes, read in order: every read past the end is zero.
pub(super) struct Bytes<'a>(pub &'a [u8]);

impl Bytes<'_> {
    pub(super) fn u8(&mut self) -> u8 {
        let Some((b, rest)) = self.0.split_first() else { return 0 };
        self.0 = rest;
        *b
    }

    pub(super) fn u16(&mut self) -> u16 {
        u16::from_le_bytes([self.u8(), self.u8()])
    }

    /// `n` bytes, zeros past the end.
    pub(super) fn take(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.u8()).collect()
    }

    /// An integer argument: the values programs treat specially (0, 1, the maximum), small ones,
    /// amounts in thousandths of a FANG, or all 64 bits.
    fn int(&mut self) -> u64 {
        match self.u8() % 8 {
            0 => 0,
            1 => 1,
            2 => u64::MAX,
            3 => u64::from(self.u8()),
            4 => u64::from(self.u16()),
            5 => u64::from(self.u16()) * (FANG / 1_000),
            6 => u64::from(self.u8()) << (self.u8() % 56),
            _ => u64::from_le_bytes(self.take(8).try_into().expect("8 bytes")),
        }
    }
}

/// Which of `n` programs a selector byte names: below 128, one of the four guests; else any.
fn pick(sel: u8, n: usize) -> usize {
    if sel < 128 { sel as usize % GUESTS.len() } else { sel as usize % n }
}

/// What a program transaction signs as its maximum fee (an op's fee byte): far above its charge, or
/// the edge of it. Admission must take the exact charge and refuse a drop less; `oracle::charge` says
/// what that is, from the spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fee {
    /// [`PROGRAM_FEE`].
    Max,
    /// Exactly the transaction's charge in the block that includes it.
    Exact,
    /// One drop less.
    Short,
    /// What a transaction without fuel pays (`Tx::charge`), below any program transaction's.
    Idle,
}

impl Fee {
    /// The fee a fee byte asks for: most are [`Fee::Max`] (and zero is), one in eight each of the others.
    fn of(byte: u8) -> Fee {
        match byte % 8 {
            1 => Fee::Exact,
            2 => Fee::Short,
            3 => Fee::Idle,
            _ => Fee::Max,
        }
    }
}

impl Sm<'_> {
    /// `action`, a program transaction, from `from`, paying `fee` at most ([`Fee`]).
    pub(super) fn program_tx(&mut self, signer: &Who, from: &str, tip: u64, fee: Fee, action: Action) {
        self.tx_paying(signer, from, tip, fee, action);
    }

    /// The programs that exist: the guests', then every other's (in address order), with their
    /// records.
    fn programs(&self) -> Vec<(String, ProgramRecord)> {
        let guests = self.fx.programs.as_ref().expect("the v2 fixture has its guests");
        let records = &self.state.programs.records;
        let mut all: Vec<(String, ProgramRecord)> = guests.addresses.iter().filter_map(|a| records.get(a).map(|r| (a.clone(), (**r).clone()))).collect();
        all.extend(records.iter().filter(|(a, _)| !guests.addresses.contains(a)).map(|(a, r)| (a.clone(), (**r).clone())));
        all
    }

    /// An address the fuzzer picks: a program (when the selector is odd), or one of the users, the
    /// validator and two newcomers, or 32 bytes of its own.
    fn address(&self, rd: &mut Bytes) -> [u8; 32] {
        let sel = rd.u8();
        let parse = |a: &str| crate::crypto::address_bytes(a).expect("an address the fixture made parses");
        if sel % 2 == 1 {
            let programs = self.programs();
            return parse(&programs[pick(sel / 2, programs.len())].0);
        }
        let fx = self.fx;
        let others: Vec<[u8; 32]> = fx.users.iter().map(|u| u.address.as_str()).chain([fx.validator.address.as_str()]).chain(fx.newcomers.iter().take(2).map(String::as_str)).map(parse).collect();
        match others.get(sel as usize / 2 % (others.len() + 1)) {
            Some(a) => *a,
            None => rd.take(32).try_into().expect("32 bytes"),
        }
    }

    /// An argument of type `ty`, encoded as the guests read theirs: integers little-endian at their
    /// width, an address as its 32 bytes, a vector as its items one after another (no length: the
    /// router's hops), an option with its tag, bytes and text with a u32 length.
    fn arg(&self, ty: &Type, rd: &mut Bytes, out: &mut Vec<u8>) {
        match ty {
            Type::U8 | Type::Bool => out.push(rd.u8()),
            Type::U16 => out.extend((rd.int() as u16).to_le_bytes()),
            Type::U32 => out.extend((rd.int() as u32).to_le_bytes()),
            Type::U64 | Type::Fang | Type::I64 => out.extend(rd.int().to_le_bytes()),
            Type::U128 | Type::I128 => out.extend((u128::from(rd.int()) << 64 | u128::from(rd.int())).to_le_bytes()),
            Type::Address => out.extend(self.address(rd)),
            Type::Hash32 => out.extend(rd.take(32)),
            Type::Name => {
                let name = format!("name{}", rd.u8() % 8);
                out.push(name.len() as u8);
                out.extend(name.bytes());
            }
            Type::Bytes { max } => {
                let n = rd.u8() as usize % (*max as usize).min(64).max(1);
                out.extend((n as u32).to_le_bytes());
                out.extend(rd.take(n));
            }
            Type::Text { max } => {
                let n = rd.u8() as usize % (*max as usize).min(64).max(1);
                out.extend((n as u32).to_le_bytes());
                out.extend((0..n).map(|_| b'a' + rd.u8() % 26));
            }
            Type::Option(inner) => {
                if rd.u8() % 2 == 1 {
                    out.push(1);
                    self.arg(inner, rd, out);
                } else {
                    out.push(0);
                }
            }
            Type::Vec { of, max } => {
                for _ in 0..rd.u8() as usize % ((*max as usize).min(8) + 1) {
                    self.arg(of, rd, out);
                }
            }
        }
    }

    /// The arguments of `method`, from the fuzzer's bytes.
    fn args(&self, method: &Method, rd: &mut Bytes) -> Vec<u8> {
        let mut out = vec![];
        for a in &method.args {
            self.arg(&a.ty, rd, &mut out);
        }
        out
    }

    /// One call, as the stream describes it: a program ([`pick`]), flags (bit 0: the input is raw
    /// bytes rather than the method's arguments; 1: a stale version digest; 2: send FANG a method
    /// doesn't take; 3: any method, not only the ones a transaction may call), a method, the value
    /// when the method takes it (or bit 2), and the input. `None` where nothing would be called.
    fn call(&self, rd: &mut Bytes) -> Option<ProgramCall> {
        let programs = self.programs();
        let (address, record) = &programs[pick(rd.u8(), programs.len())];
        let flags = rd.u8();
        let interface = self.state.programs.interfaces.get(&record.interface_hash)?.interface()?;
        let callable: Vec<&Method> = interface.methods.iter().filter(|m| m.kind == MethodKind::Call).collect();
        let sel = rd.u8() as usize;
        let method = if callable.is_empty() || flags & 8 != 0 { &interface.methods[sel % interface.methods.len()] } else { callable[sel % callable.len()] };
        let value = if method.payable || flags & 4 != 0 { u64::from(rd.u16()) * (FANG / 1_000) } else { 0 };
        let input = if flags & 1 != 0 {
            let n = rd.u8() as usize % 49;
            rd.take(n)
        } else {
            self.args(method, rd)
        };
        let mut version = digest::version_digest(address, &record.wasm_hash, &record.interface_hash, record.version);
        if flags & 2 != 0 {
            version[0] ^= 1;
        }
        Some(ProgramCall { program: address.clone(), version_digest: hex::encode(version), method: method.name.clone(), input: HexBytes(input), value })
    }

    /// A transaction's fuel: a tiny amount, the most a transaction may ever sign for, the most the
    /// chain lets this one sign for (`program_max_tx_fuel`) or half the block's budget, so that two
    /// or three fill a block and the budget's edge is within reach, or `m × 2^e` (`m` 1–256, `e`
    /// 0–14), so a run goes out of fuel as often as it finishes.
    fn fuel(&self, rd: &mut Bytes) -> u64 {
        let (max_tx, budget) = (params::param(&self.state, params::Param::MaxTxFuel), params::param(&self.state, params::Param::MaxBlockFuel));
        match rd.u8() % 32 {
            0 => MAX_TX_FUEL,
            1 => 1,
            30 => (budget / 2).clamp(1, max_tx),
            31 => max_tx,
            _ => (u64::from(rd.u8()) + 1) << (rd.u8() % 15),
        }
    }

    /// The most a transaction's deposits may bond: up to 6.5 FANG in 0.0001 steps.
    fn storage_limit(rd: &mut Bytes) -> u64 {
        u64::from(rd.u16()) * 100_000
    }

    /// Bindings: bit `i` of a mask byte binds the `i`th of the programs (the guests first), unless a
    /// call names it already; a second byte's bit 0 binds each at its version in force rather than
    /// any.
    fn bindings(&self, rd: &mut Bytes, calls: &[ProgramCall]) -> Vec<Binding> {
        let (mask, exact) = (rd.u8(), rd.u8() & 1 == 1);
        let programs = self.programs();
        programs
            .iter()
            .take(8)
            .enumerate()
            .filter(|(i, (address, _))| mask >> i & 1 == 1 && !calls.iter().any(|c| c.program == *address))
            .take(MAX_BINDINGS)
            .map(|(_, (address, r))| {
                let version_digest = if exact { hex::encode(digest::version_digest(address, &r.wasm_hash, &r.interface_hash, r.version)) } else { "00".repeat(32) };
                Binding { program: address.clone(), version_digest }
            })
            .collect()
    }

    /// A `CreateProgram`, as the stream describes it (the module docs).
    fn create(&self, rd: &mut Bytes) -> Option<Action> {
        let flags = rd.u8();
        let (code, iface) = if flags & 1 == 0 {
            let g = &GUESTS[rd.u8() as usize % GUESTS.len()];
            (digest::wasm_hash(g.wasm), digest::interface_hash(g.iface))
        } else {
            // Any stored code with any stored interface: a pair that doesn't match is refused.
            let (codes, ifaces): (Vec<[u8; 32]>, Vec<[u8; 32]>) = (self.state.programs.code.keys().copied().collect(), self.state.programs.interfaces.keys().copied().collect());
            if codes.is_empty() || ifaces.is_empty() {
                return None;
            }
            (codes[rd.u8() as usize % codes.len()], ifaces[rd.u8() as usize % ifaces.len()])
        };
        let interface = self.state.programs.interfaces.get(&iface)?.interface()?;
        let init = match (interface.init(), flags & 2 != 0) {
            (Some(m), false) => Some(InitCall { method: m.name.clone(), input: HexBytes(self.args(m, rd)), value: if m.payable { u64::from(rd.u16()) * (FANG / 1_000) } else { 0 } }),
            // An initializer the interface doesn't declare, or one left out: refused.
            (None, true) => Some(InitCall { method: "init".into(), input: HexBytes(vec![]), value: 0 }),
            _ => None,
        };
        let authority = (flags & 4 != 0).then(|| self.fx.users[rd.u8() as usize % SM_USERS].address.clone());
        let least = params::param(&self.state, params::Param::MinUpgradeDelayBlocks);
        let upgrade_delay_blocks = match (authority.is_some(), rd.u8() % 4) {
            (false, _) => 0,
            (true, 0) => least.saturating_sub(1),
            (true, 1) => least + u64::from(rd.u16()),
            (true, _) => least,
        };
        let policy = if authority.is_some() && flags & 8 != 0 { ProgramPolicy::Additive } else { ProgramPolicy::Open };
        let (fuel_limit, bindings) = if init.is_some() { (self.fuel(rd), self.bindings(rd, &[])) } else { (0, vec![]) };
        let storage_limit = (1 + u64::from(rd.u16())) * 1_000_000;
        Some(Action::CreateProgram { wasm_hash: hex::encode(code), interface_hash: hex::encode(iface), authority, upgrade_delay_blocks, policy, accepts_transfers: flags & 16 != 0, init, fuel_limit, storage_limit, bindings })
    }

    /// An `UploadCode`: a guest's code and interface, with a few of the fuzzer's edits to each, and
    /// perhaps another guest's interface (which doesn't match).
    fn upload(&self, rd: &mut Bytes) -> Action {
        let g = &GUESTS[rd.u8() as usize % GUESTS.len()];
        let (mut wasm, mut iface) = (g.wasm.to_vec(), g.iface.to_vec());
        let flags = rd.u8();
        if flags & 1 != 0 {
            iface = GUESTS[rd.u8() as usize % GUESTS.len()].iface.to_vec();
        }
        edit(&mut wasm, rd);
        if flags & 2 != 0 {
            edit(&mut iface, rd);
        }
        let storage_limit = (1 + u64::from(rd.u16())) * 100_000;
        Action::UploadCode { wasm: HexBytes(wasm), interface: HexBytes(iface), storage_limit }
    }
}

/// Up to three edits to `bytes`: a byte changed, the end cut off, or bytes added at the end.
fn edit(bytes: &mut Vec<u8>, rd: &mut Bytes) {
    for _ in 0..rd.u8() % 4 {
        match rd.u8() % 4 {
            0 | 1 if !bytes.is_empty() => {
                let at = rd.u16() as usize % bytes.len();
                bytes[at] ^= rd.u8().max(1);
            }
            2 => {
                let n = rd.u16() as usize % (bytes.len() + 1);
                bytes.truncate(n);
            }
            _ => {
                let n = rd.u8() as usize % 8;
                bytes.extend(rd.take(n));
            }
        }
    }
}

/// Interprets program op `kind` (40–46): the module docs.
pub(super) fn op(sm: &mut Sm<'_>, kind: u8, a: u8, b: u8, amt: u64, payload: &[u8]) {
    let fx = sm.fx;
    let stream = [&[b, amt as u8, (amt >> 8) as u8][..], payload].concat();
    let mut rd = Bytes(&stream);
    let (user, tip) = (&fx.users[a as usize % SM_USERS], u64::from(a) / SM_USERS as u64 % 4);
    let from = user.address.clone();
    // An input that has asked for all the fuel it may signs no more: its work is bounded (FUEL_CAP).
    if matches!(kind, OP_CALL | OP_BATCH | OP_CREATE | OP_UPLOAD) && sm.fuel_signed > FUEL_CAP {
        return;
    }
    match kind {
        OP_CALL | OP_BATCH => {
            let n = if kind == OP_BATCH { 1 + rd.u8() as usize % MAX_CALLS } else { 1 };
            let calls: Vec<ProgramCall> = (0..n).filter_map(|_| sm.call(&mut rd)).collect();
            if calls.is_empty() {
                return;
            }
            let (fuel_limit, storage_limit) = (sm.fuel(&mut rd), Sm::storage_limit(&mut rd));
            let bindings = sm.bindings(&mut rd, &calls);
            let fee = Fee::of(rd.u8());
            sm.fuel_signed += fuel_limit;
            sm.program_tx(user, &from, tip, fee, Action::CallProgram { calls, fuel_limit, storage_limit, bindings });
        }
        OP_CREATE => {
            if let Some(create) = sm.create(&mut rd) {
                let fee = Fee::of(rd.u8());
                if let Action::CreateProgram { fuel_limit, .. } = &create {
                    sm.fuel_signed += fuel_limit;
                }
                sm.program_tx(user, &from, tip, fee, create);
            }
        }
        OP_UPLOAD => {
            let upload = sm.upload(&mut rd);
            let fee = Fee::of(rd.u8());
            sm.fuel_signed += oracle::exec_fuel_of(&upload);
            sm.program_tx(user, &from, tip, fee, upload);
        }
        OP_PAUSE => {
            // A genesis validator, a key a user registered a validator with (signing from its own
            // account), or a user (for the validator it owns).
            let signers: Vec<&Who> = fx.validators.iter().chain(&fx.newcomer_keys).chain(&fx.users).collect();
            let who = signers[a as usize % signers.len()];
            sm.tx(who, &who.address.clone(), tip, Action::ProgramsPause {});
        }
        OP_PAY => {
            let programs = sm.programs();
            let creator = crate::crypto::address_bytes(&from).expect("a user's address parses");
            // The program's account, or the address this user's next create (or the one after)
            // will make: FANG sent ahead, which that create adopts.
            let next = sm.next_nonce(&from);
            let to = match rd.u8() % 4 {
                0 | 1 => programs[pick(rd.u8(), programs.len())].0.clone(),
                n => digest::program_address(sm.state.params.prefix(), &creator, next + u64::from(n - 2)),
            };
            // Up to 655 FANG, in steps of 0.01.
            let amount = u64::from(rd.u16()) * (FANG / 100);
            sm.tx(user, &from, tip, Action::Transfer { to, amount });
        }
        OP_GOV => governance(sm, b, amt),
        _ => {}
    }
}

/// What the governance executor does at the end of a block when a proposal passes (`upgrade`'s
/// levers and `params::set_param`, which `State::exec_set_param` calls), done at a block boundary:
/// the block in progress ends first, as the proposal's own block does. Switch programs off
/// (exit-only: with transactions in flight, balances held, a pause maybe running), on again
/// (`on_activate` must leave programs' state alone), or set one of the parameters, to a value
/// within its bounds or not (the executor refuses one that isn't); or let blocks pass.
fn governance(sm: &mut Sm<'_>, b: u8, amt: u64) {
    sm.end_block();
    if sm.halted {
        return;
    }
    let mut j = crate::chain::Journal::default();
    let state = &mut sm.state;
    let feature = crate::upgrade::PROGRAMS_FEATURE;
    let (key, value) = match b % 9 {
        0 => {
            let _ = crate::upgrade::exec_deactivate_feature(state, &mut j, feature);
            return;
        }
        1 => {
            let meta = state.programs.meta.clone();
            if crate::upgrade::exec_activate_feature(state, &mut j, sm.fx.chain, feature, sm.height).is_ok() {
                assert_eq!(meta, state.programs.meta, "switching programs on again changed their state: on_activate must leave META alone");
            }
            return;
        }
        2 => (params::Param::ValueCap, [0, 100 * FANG, 500 * FANG, 10_000_000 * FANG][amt as usize % 4]),
        3 => (params::Param::MaxBlockFuel, params::MIN_BLOCK_FUEL + (amt % 3) * 20_000_000),
        4 => (params::Param::MaxTxFuel, 1_000_000 + (amt % 4) * 3_000_000),
        5 => (params::Param::FuelFloorRate, [1, 250, 5_000, 1_000_000][amt as usize % 4]),
        6 => (params::Param::StoragePrice, [1_000, 100_000, 10_000_000][amt as usize % 3]),
        7 => (params::Param::ValueCap, amt * FANG),
        // Let up to 63 blocks pass: a pause's spans are days, which the pause profile has few
        // blocks of, but a few hundred empty blocks cross them. All but the last are quick.
        _ => {
            let n = amt % 64;
            sm.quick = true;
            for i in 0..n {
                sm.quick = i + 1 < n;
                sm.end_block();
            }
            sm.quick = false;
            return;
        }
    };
    let _ = params::set_param(state, &mut j, key.key(), value);
}

// ----------------------------------------------------------------------------- the block's checks

/// What a block on the v2 fixture ran, for [`check_block`]: the transactions it included (those
/// `State::apply` didn't refuse) and which of them failed, as `Chain::apply_block` hands them to
/// `programs::receipt::for_block`.
pub(super) struct Ran {
    pub(super) included: Vec<Tx>,
    pub(super) failed: BTreeMap<usize, String>,
    /// The FANG programs control before the block.
    control: u128,
    /// What the block's program transactions add up to (`oracle`).
    model: oracle::BlockModel,
}

/// A fault of this machine, not of the node's code: it is not a finding (R8).
pub(super) struct Resource;

/// Whether the state faulted: a **Tx**-class fault is a finding (no valid transaction can cause
/// one, §6.2, and a node that met one halts), so it panics naming the transaction and class; a
/// **Setup**-class fault (the OS would not start a program thread, the engine would not build) is
/// the machine's and says nothing of the code, so it's a [`Resource`] error.
pub(super) fn fault_of(state: &State, height: u64) -> Result<(), Resource> {
    match &state.programs.fault {
        None => Ok(()),
        Some(f) if f.class == FaultClass::Setup => Err(Resource),
        Some(f) => panic!("a program transaction faulted the node at block {height}: tx {} ({:?}): {}", f.tx, f.class, f.what),
    }
}

/// A block of the v2 fixture, run unrolled: `State::run_block` opened up to look at the state
/// around each program transaction. It must keep exactly the calls `Chain::run_block` makes
/// (chain.rs `State::run_block`), in its order: `version_at(height)` once before the loop; per
/// transaction `tx.size()` into `tx_bytes`, `State::apply` (a refusal is `skipped`); then
/// `State::end_block` with the block's `BlockInfo` (view = height, parent view = height − 1). The
/// determinism rerun of a quarter of the inputs runs the block through `State::run_block` itself
/// and holds the two to one root and one digest of the block's receipts.
///
/// A Setup-class fault (a thread the OS wouldn't start) gets one retry of the block from its start;
/// if it comes again the machine can't run the input and it ends there (`None`).
pub(super) fn run_block(sm: &mut Sm<'_>, height: u64, hash: &str, proposer: &str, txs: &[Tx], applied: &mut usize) -> Option<Ran> {
    let (start, guests) = (sm.state.clone(), sm.guests.clone());
    for _ in 0..2 {
        match unrolled(sm, height, hash, proposer, txs) {
            Ok((ran, n)) => {
                *applied += n;
                return Some(ran);
            }
            Err(Resource) => (sm.state, sm.guests) = (start.clone(), guests.clone()),
        }
    }
    None
}

fn unrolled(sm: &mut Sm<'_>, height: u64, hash: &str, proposer: &str, txs: &[Tx]) -> Result<(Ran, usize), Resource> {
    let chain = sm.fx.chain;
    let version = crate::upgrade::version_at(&sm.state, height);
    let (mut skipped, mut tx_bytes, mut applied) = (vec![], 0u64, 0usize);
    let mut ran = Ran { included: vec![], failed: BTreeMap::new(), control: programs::commit::control(&sm.state), model: oracle::BlockModel::start(&sm.state) };
    for tx in txs {
        let size = tx.size();
        tx_bytes += size;
        let pre = oracle::is_program_tx(tx).then(|| sm.state.clone());
        match sm.state.apply(tx, chain, proposer, height) {
            Err(why) => {
                skipped.push(size);
                if let Some(pre) = &pre {
                    oracle::check_refused(pre, tx, &why, height, &ran.model);
                    sm.refused.push(format!("{why:#}"));
                }
            }
            Ok(outcome) => {
                fault_of(&sm.state, height)?;
                if let Some(tamper) = sm.tamper {
                    tamper(&mut sm.state, tx);
                }
                applied += matches!(outcome, Outcome::Applied) as usize;
                if let Some(pre) = &pre {
                    let run = &sm.state.programs.block;
                    let receipt = run.receipts.last().filter(|(id, _)| run.height == height && *id == tx.id()).map(|(_, r)| r.clone());
                    oracle::check_ran(&oracle::TxRun { pre, post: &sm.state, tx, outcome: &outcome, receipt: receipt.as_ref(), chain, proposer, height }, &mut ran.model);
                    if outcome == Outcome::Applied {
                        sm.guests.applied(&sm.state, tx);
                    }
                }
                if let Outcome::Failed(why) = outcome {
                    ran.failed.insert(ran.included.len(), why);
                }
                ran.included.push(tx.clone());
            }
        }
        fault_of(&sm.state, height)?;
    }
    sm.state.end_block(&BlockInfo { chain_id: chain, height, version, hash, proposer, view: height, parent_view: height - 1, skipped: &skipped, tx_bytes });
    Ok((ran, applied))
}

/// The block's checks after its state is committed: no node fault; programs' totals; the oracles'
/// (the fuel it counted and the rate it left, program accounts' balances, a pause's run, the
/// guests' storage against their models); the training wheels; and its receipts (`for_block`, as
/// `Chain::apply_block` takes them) against its transactions and each other. Returns the digest of
/// their JSON, for the determinism check.
pub(super) fn check_block(sm: &mut Sm<'_>, height: u64, ran: Ran) -> [u8; 32] {
    assert!(programs::check_totals(&sm.state), "program accounting drifted at block {height}");
    oracle::check_block_end(&sm.state, height, &ran.model, &mut sm.guests.last_run);
    sm.guests.check(&sm.state, height);
    // The training wheels (§7.1, §7.3): no block raises the FANG programs control past the value cap
    // (a cap lowered under what programs hold only stops it rising), and while programs are
    // exit-only none raises it at all.
    let (control, cap) = (programs::commit::control(&sm.state), u128::from(params::param(&sm.state, params::Param::ValueCap)));
    assert!(control <= ran.control.max(cap), "block {height} took the FANG programs control from {} to {control}, past the value cap of {cap}", ran.control);
    assert!(control <= ran.control || programs::status(&sm.state, height) != programs::Status::ExitOnly, "block {height} raised the FANG programs control from {} to {control} while programs were exit-only", ran.control);
    let budget = params::param(&sm.state, params::Param::MaxBlockFuel);
    assert!(ran.model.counted <= ran.model.limit.max(budget), "block {height} counted {} fuel past its budget of {}", ran.model.counted, ran.model.limit);
    let receipts = programs::receipt::for_block(&mut sm.state, height, &ran.included, &ran.failed);
    for (&i, r) in &receipts {
        let tx = ran.included.get(i).unwrap_or_else(|| panic!("a receipt for transaction {i} of a block of {}", ran.included.len()));
        let (kind, calls) = match &tx.action {
            Action::UploadCode { .. } => (programs::receipt::ReceiptKind::Upload, 0),
            Action::CreateProgram { init, .. } => (programs::receipt::ReceiptKind::Create, init.iter().count()),
            Action::CallProgram { calls, .. } => (programs::receipt::ReceiptKind::Call, calls.len()),
            a => panic!("a receipt for a {} transaction", a.kind()),
        };
        assert_eq!((r.kind, r.calls.len()), (kind, calls), "the receipt of a {} says {:?} with {} calls", tx.action.kind(), r.kind, r.calls.len());
        assert_eq!(r.outcome.is_ok(), !ran.failed.contains_key(&i), "a {} {} while its receipt says {}", tx.action.kind(), if ran.failed.contains_key(&i) { "failed" } else { "applied" }, r.outcome.code());
        if r.outcome.is_ok() {
            assert!(kind != programs::receipt::ReceiptKind::Create || r.created.is_some(), "an applied create made no program");
            assert!(kind != programs::receipt::ReceiptKind::Upload || r.upload.is_some(), "an applied upload stored nothing");
        } else {
            assert_eq!(*r, empty(r), "a failed receipt shows something of its action: {r:?}");
        }
        *sm.seen.entry((tx.action.kind(), r.outcome.code())).or_default() += 1;
        sm.kept.entry((tx.action.kind(), r.outcome.code())).or_insert_with(|| r.clone());
        if r.burned_by_programs > 0 {
            sm.kept.entry((tx.action.kind(), "burned")).or_insert_with(|| r.clone());
        }
    }
    // Display only, but served as JSON from the block log: it must read back as it was.
    let json = serde_json::to_vec(&receipts).expect("receipts serialize");
    let back: BTreeMap<usize, ProgramReceipt> = serde_json::from_slice(&json).expect("a block's receipts must read back from their JSON");
    assert_eq!(back, receipts, "the receipts' JSON round trip changed them");
    crate::crypto::hash(&json)
}

/// The digest of the receipts of a block that `State::run_block` ran (the determinism rerun): the
/// same bytes [`check_block`] hashes for the same receipts.
pub(super) fn digest_block(sm: &mut Sm<'_>, height: u64, included: &[Tx], failed: &BTreeMap<usize, String>) -> [u8; 32] {
    let receipts = programs::receipt::for_block(&mut sm.state, height, included, failed);
    crate::crypto::hash(&serde_json::to_vec(&receipts).expect("receipts serialize"))
}

/// The digest of the receipts of a block that has none (`{}`), for the blocks made quickly.
pub(super) fn empty_digest() -> [u8; 32] {
    crate::crypto::hash(b"{}")
}

/// `r` with everything a failed receipt doesn't keep taken away: what it paid and the fuel it
/// used stay, and its calls keep no return data.
fn empty(r: &ProgramReceipt) -> ProgramReceipt {
    let calls = r.calls.iter().map(|c| programs::receipt::CallResult { return_data: HexBytes::default(), ..c.clone() }).collect();
    ProgramReceipt { calls, events: vec![], omitted: None, balance_changes: vec![], storage: Default::default(), outside_vm: Default::default(), burned_by_programs: 0, created: None, upload: None, ..r.clone() }
}

// ----------------------------------------------------------------------------------------- seeds

/// One op of a seed, as its kind, `a`, and its stream (the bytes it reads in order: `b`, the amount's
/// two, then the payload).
type SeedOp = (u8, u8, Vec<u8>);

/// A `state_machine_programs` input on `profile`: its first byte, then the ops, a program op with
/// its length byte and payload.
fn input(profile: Profile, ops: &[SeedOp]) -> Vec<u8> {
    let mut out = vec![profile.byte()];
    for (kind, a, stream) in ops {
        let mut stream = stream.clone();
        stream.resize(stream.len().max(3), 0);
        out.extend([*kind, *a, stream[0], stream[1], stream[2]]);
        if (OP_CALL..=OP_GOV).contains(kind) {
            out.push((stream.len() - 3) as u8);
            out.extend(&stream[3..]);
        }
    }
    out
}

/// The guests' places among the programs, as an address argument names them (an odd selector `2i +
/// 1`) and as a call does (`i`).
const COUNTER: u8 = 0;
const TOKEN: u8 = 1;
const ESCROW: u8 = 2;
const ROUTER: u8 = 3;

/// An address argument that names the first newcomer, an address that holds nothing.
const NEWCOMER: u8 = 14;

/// An address argument that names guest `i`, or user `i`.
fn program(i: u8) -> u8 {
    2 * i + 1
}

fn user(i: u8) -> u8 {
    2 * i
}

/// An integer argument: `n` as a `u16` (selector 4).
fn small(n: u16) -> Vec<u8> {
    vec![4, n as u8, (n >> 8) as u8]
}

/// A call's stream (op 40's): the guest, the flags, the method (an index among the ones a
/// transaction may call), the value (when it takes one, in thousandths of a FANG), the arguments,
/// then the fuel (byte 2, then 256 × 2^10), the storage limit (20,000 × 0.0001 FANG), the
/// bindings' mask (bit `i`: guest `i`) and whether they're exact, and the fee byte.
fn call(guest: u8, method: u8, value: Option<u16>, args: &[u8], bindings: u8) -> Vec<u8> {
    let value = value.map(u16::to_le_bytes);
    [&[guest, 0, method][..], value.as_slice().concat().as_slice(), args, &[2, 255, 10, 0x20, 0x4e, bindings, 0, 0]].concat()
}

/// [`call`] with its fuel bytes (the first picks the amount: [`Sm::fuel`]) and fee byte ([`Fee::of`]).
fn call_with(guest: u8, method: u8, value: Option<u16>, args: &[u8], bindings: u8, fuel: &[u8], fee: u8) -> Vec<u8> {
    let value = value.map(u16::to_le_bytes);
    [&[guest, 0, method][..], value.as_slice().concat().as_slice(), args, fuel, &[0x20, 0x4e, bindings, 0, fee]].concat()
}

fn seed_op(kind: u8, a: u8, stream: Vec<u8>) -> SeedOp {
    (kind, a, stream)
}

/// The op that ends the block (v1's 34).
fn end() -> SeedOp {
    seed_op(34, 0, vec![])
}

/// The `state_machine` seeds of the v2 fixture: every kind of program op, and each of the
/// fixture's three profiles. Named after what they do; `tests/harness.rs` holds them to it.
pub(super) fn seeds() -> Vec<(&'static str, Vec<u8>)> {
    let increment = || seed_op(OP_CALL, 1, call(COUNTER, 0, None, &[], 0));
    // token.transfer(to, amount) from user `a`.
    let transfer = |a: u8, to: u8, amount: u16| seed_op(OP_CALL, a, call(TOKEN, 0, None, &[&[to][..], &small(amount)].concat(), 0));
    // escrow.open(id, seller) with `value` thousandths of a FANG; release(id); refund(id). The
    // callable methods ascend by name: open, refund, release.
    let open = |a: u8, id: u16, seller: u8, value: u16| seed_op(OP_CALL, a, call(ESCROW, 0, Some(value), &[&small(id)[..], &[seller]].concat(), 0));
    let refund = |a: u8, id: u16| seed_op(OP_CALL, a, call(ESCROW, 1, None, &small(id), 0));
    let release = |a: u8, id: u16| seed_op(OP_CALL, a, call(ESCROW, 2, None, &small(id), 0));
    // router.send(hops, token, to, amount) with the token bound, and try_send(token, to, amount).
    let send = |a: u8, hops: u8, to: u8, amount: u16| seed_op(OP_CALL, a, call(ROUTER, 0, None, &[&[hops, program(TOKEN), to][..], &small(amount)].concat(), 1 << TOKEN));
    let try_send = |a: u8, to: u8, amount: u16| seed_op(OP_CALL, a, call(ROUTER, 1, None, &[&[program(TOKEN), to][..], &small(amount)].concat(), 1 << TOKEN));
    // A create: its flags and the blobs (a guest), what its authority and `init` read, and its
    // storage limit (300 × 1 FANG's millionth: 0.03 FANG covers the record's deposit and a few entries).
    let create = |a: u8, flags: u8, guest: u8, rest: &[u8]| seed_op(OP_CREATE, a, [&[flags, guest][..], rest, &[0x2c, 0x01]].concat());
    let pay = |a: u8, to: u8, steps: u16| seed_op(OP_PAY, a, [&[to, 0][..], &steps.to_le_bytes()].concat());
    // An upload of a guest's code and interface, the edits to the code (a count, then each) and a
    // storage limit of 0.1 FANG.
    let upload = |guest: u8, flags: u8, edits: &[u8]| seed_op(OP_UPLOAD, 1, [&[guest, flags][..], edits, &[0xe8, 0x03]].concat());
    // A batch (op 41) of three calls from user `a`: the counter twice, then a transfer of the token.
    let batch = |a: u8| seed_op(OP_BATCH, a, [&[2u8][..], &[COUNTER, 0, 0], &[COUNTER, 0, 0], &[TOKEN, 0, 0, user(1)], &small(1), &[2, 255, 10, 0x20, 0x4e, 0, 0, 0]].concat());
    // A batch (op 41) of escrow methods (`method` 1 refund, 2 release) on the deals `ids`, in one transaction.
    let settle_batch = |a: u8, method: u8, ids: &[u16]| {
        let calls: Vec<u8> = ids.iter().flat_map(|id| [&[ESCROW, 0, method][..], &small(*id)[..]].concat()).collect();
        seed_op(OP_BATCH, a, [&[ids.len() as u8 - 1][..], &calls, &[2, 255, 14, 0x20, 0x4e, 0, 0, 0]].concat())
    };
    // A batch of `n` increments of the counter.
    let batch_of = |a: u8, n: u8| seed_op(OP_BATCH, a, [&[n - 1][..], &(0..n).flat_map(|_| [COUNTER, 0, 0]).collect::<Vec<u8>>(), &[2, 255, 14, 0x20, 0x4e, 0, 0, 0]].concat());
    // An increment that signs the fee byte `fee` ([`Fee::of`]: 1 the exact charge, 2 a drop less, 3 what a transaction without fuel pays).
    let increment_fee = |fee: u8| seed_op(OP_CALL, 1, call_with(COUNTER, 0, None, &[], 0, &[2, 255, 10], fee));
    // An increment that signs for `fuel`'s first byte (30: half the block's budget, at most `program_max_tx_fuel`; 31: that).
    let increment_fuel = |a: u8, fuel: u8| seed_op(OP_CALL, a, call_with(COUNTER, 0, None, &[], 0, &[fuel], 0));
    // A governance step (op 46): `b` picks it ([`governance`]), `amt` its value.
    let gov = |b: u8, amt: u16| seed_op(OP_GOV, 0, vec![b, amt as u8, (amt >> 8) as u8]);
    // A pause signature from the `a`th of the signers (the genesis validators first).
    let pause = |a: u8| seed_op(OP_PAUSE, a, vec![]);
    // Blocks passing: `n` of them in one step.
    let blocks = |n: u16| gov(8, n);
    let mut seeds = vec![];
    // The counter and the token: calls that write storage, transfers between users, one to the router.
    seeds.push(("programs-counter-and-token", input(Profile::Default, &[increment(), increment(), end(), increment(), transfer(0, user(1), 100), transfer(1, user(0), 40), transfer(0, program(ROUTER), 500), transfer(2, user(0), 1)])));
    // The router: it holds tokens, and sends them on with no hops, one hop, and through try_send.
    seeds.push(("programs-router", input(Profile::Default, &[transfer(0, program(ROUTER), 500), end(), send(1, 0, user(2), 5), try_send(1, user(2), 5), try_send(1, user(2), 5_000), send(1, 1, user(2), 5)])));
    // The escrow: deals opened, two released (one to a seller who has no account yet, which the
    // release opens, so its fee is burned out of the program's send), one refunded, and a release
    // that finds nothing.
    seeds.push(("programs-escrow", input(Profile::Default, &[open(2, 1, user(3), 5_000), open(2, 2, user(4), 3_000), open(2, 3, NEWCOMER, 5_000), end(), release(2, 1), refund(4, 2), release(2, 3), end(), release(2, 1)])));
    // Batches: the first lands, the second's transfer finds no tokens, so none of it does.
    seeds.push(("programs-batch", input(Profile::Default, &[batch(0), end(), batch(2)])));
    // Creates (a second counter that accepts transfers, a token with `init`, an upgradable program,
    // an `init` the interface doesn't declare), FANG sent ahead to an address a create adopts, then
    // calls to the programs by index among them, and uploads: the counter's code again, with a byte
    // changed that keeps it valid, and one that doesn't, then with another guest's interface.
    seeds.push((
        "programs-create-pay-upload",
        input(
            Profile::Default,
            &[
                pay(0, 2, 1_000),
                create(0, 16, COUNTER, &[0]),
                create(0, 0, TOKEN, &[user(0), 4, 0xe8, 0x03, 0, 2, 255, 10, 0, 0]),
                create(0, 4 | 8 | 16, COUNTER, &[1, 2]),
                create(0, 2, COUNTER, &[0, 2, 255, 10, 0, 0]),
                end(),
                seed_op(OP_CALL, 0, call(129, 0, None, &[], 0)),
                seed_op(OP_CALL, 0, call(130, 0, None, &[], 0)),
                pay(3, 0, 1_000),
                upload(COUNTER, 0, &[0]),
                upload(COUNTER, 0, &[1, 1, 10, 0, 1]),
                upload(COUNTER, 0, &[1, 1, 0x2c, 0x01, 0x55]),
                upload(COUNTER, 1, &[TOKEN, 0]),
                end(),
                upload(ROUTER, 0, &[2, 0, 7, 0, 0x01, 2, 0x30, 0x00]),
            ],
        ),
    ));
    // A validator's pause: nothing runs while it lasts, and nothing is paid into a program.
    seeds.push(("programs-pause", input(Profile::Default, &[increment(), seed_op(OP_PAUSE, 0, vec![]), end(), increment(), transfer(0, user(1), 1), pay(3, 0, 1_000)])));
    // The tight profile: nine deals of 65 FANG in a block (the escrow takes them in, up to the cap of
    // 500 FANG), and a payment into the counter past it.
    let deals: Vec<SeedOp> = (1..=9).map(|id| open(2, id, user(3), 65_000)).collect();
    seeds.push(("programs-value-cap", input(Profile::Tight, &[&deals[..], &[end(), pay(4, 0, 0xffff), release(2, 1), end(), pay(4, 0, 1_000)]].concat())));
    // Exit-only: a call without FANG runs, one that sends some doesn't, nor an upload, a create or a transfer in.
    seeds.push(("programs-exit-only", input(Profile::ExitOnly, &[increment(), open(2, 1, user(3), 5_000), upload(COUNTER, 0, &[0]), create(0, 16, COUNTER, &[0]), pay(3, 0, 1_000), end(), increment()])));
    // One transaction settles several deals, paying several accounts: two released to two sellers, one
    // of them a seller who has no account yet (so a payout is cut by the creation fee), then a batch of
    // two refunds, which only one seller could ask for: it aborts whole.
    seeds.push((
        "programs-escrow-batch",
        input(Profile::Default, &[open(2, 1, user(3), 5_000), open(2, 2, user(4), 3_000), open(2, 3, NEWCOMER, 4_000), open(2, 4, user(3), 2_000), open(2, 5, user(4), 1_000), end(), settle_batch(2, 2, &[1, 2, 3]), end(), seed_op(OP_BATCH, 3, [&[1u8][..], &[ESCROW, 0, 1], &small(4), &[ESCROW, 0, 1], &small(5), &[2, 255, 14, 0x20, 0x4e, 0, 0, 0]].concat()), end()]),
    ));
    // Batches of every size, 1 to 8: a fault that needs exactly so many calls is in reach of a seed.
    let sizes: Vec<SeedOp> = (1..=8).map(|n| batch_of(0, n)).collect();
    seeds.push(("programs-batch-sizes", input(Profile::Default, &[&sizes[..], &[end(), increment()]].concat())));
    // The fee's edge: the exact charge applies, a drop less is refused, and so is what an idle chain
    // charges; for a call, an upload and a create.
    seeds.push((
        "programs-fee-edge",
        input(
            Profile::Default,
            &[
                increment_fee(1),
                increment_fee(2),
                increment_fee(0),
                increment_fee(3),
                seed_op(OP_UPLOAD, 1, [&[COUNTER, 0, 0][..], &[0xe8, 0x03, 1]].concat()),
                seed_op(OP_UPLOAD, 1, [&[COUNTER, 0, 0][..], &[0xe8, 0x03, 2]].concat()),
                seed_op(OP_CREATE, 0, [&[16, COUNTER][..], &[0], &[0x2c, 0x01, 1]].concat()),
                seed_op(OP_CREATE, 0, [&[16, COUNTER][..], &[0], &[0x2c, 0x01, 2]].concat()),
                end(),
                increment_fee(1),
            ],
        ),
    ));
    // The block's fuel (the tight profile's is 20 million): a call for all `program_max_tx_fuel`
    // (10 million) fits once, a second doesn't; so in the next block with half the budget (an input
    // signs for 40 million of fuel at most, so that is the last of it); and a call for one unit of
    // fuel runs out.
    seeds.push((
        "programs-fuel-budget",
        input(Profile::Tight, &[increment_fuel(0, 31), increment_fuel(1, 31), end(), increment_fuel(0, 30), increment_fuel(1, 30), end(), increment_fuel(0, 1)]),
    ));
    // Governance: programs go exit-only with deals open and calls in flight, come back, and their
    // parameters move (the value cap to nothing, the fuel floor up, the block budget down).
    seeds.push((
        "programs-governance",
        input(
            Profile::Default,
            &[
                open(2, 1, user(3), 5_000),
                open(2, 2, user(4), 3_000),
                end(),
                gov(0, 0),
                increment(),
                open(2, 3, user(3), 1_000),
                release(2, 1),
                pay(3, 0, 1_000),
                end(),
                gov(1, 0),
                open(2, 3, user(3), 1_000),
                end(),
                gov(2, 0),
                open(2, 4, user(3), 1_000),
                increment(),
                release(2, 3),
                refund(4, 2),
                end(),
                gov(7, 3_000),
                gov(5, 3),
                gov(3, 0),
                increment(),
                open(2, 5, user(4), 2_000),
                end(),
            ],
        ),
    ));
    // The pause over its lifetime: four validators, a day of 10 blocks. One signature isn't enough
    // and expires with its day; two pause programs for 30 blocks; a pause can't be signed again
    // inside the validators' window of 300 blocks, nor a run started before 600 after the last's
    // start; after both a new run starts, and renews while it runs.
    seeds.push((
        "programs-pause-lifetime",
        input(
            Profile::Pause,
            &[
                increment(),
                pause(0),
                end(),
                increment(),
                blocks(12),
                pause(1),
                end(),
                pause(0),
                end(),
                increment(),
                blocks(20),
                increment(),
                blocks(31),
                increment(),
                pause(0),
                pause(1),
                blocks(40),
                blocks(63),
                blocks(63),
                blocks(63),
                blocks(63),
                blocks(63),
                pause(2),
                pause(3),
                blocks(63),
                blocks(63),
                blocks(63),
                blocks(63),
                pause(2),
                pause(3),
                end(),
                pause(0),
                pause(1),
                end(),
                increment(),
                blocks(40),
                increment(),
                blocks(40),
                increment(),
            ],
        ),
    ));
    // A long input, about 1.5 KB, of every
    // kind of program op in turn (a deterministic stream), so the fuzzer starts with sequences that
    // need depth: creates, then calls to the programs they made, deals opened across blocks and
    // settled in others.
    seeds.push(("programs-long", input(Profile::Default, &long_ops())));
    seeds
}

/// The long seed's ops: 100 of them from a fixed xorshift stream, each a program op picked by it,
/// with a block ended every few.
fn long_ops() -> Vec<SeedOp> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n
    };
    let mut ops = vec![];
    let mut deal = 0u16;
    for i in 0..100u64 {
        let a = next(6) as u8;
        let op = match next(12) {
            0 | 1 => seed_op(OP_CALL, a, call(COUNTER, 0, None, &[], 0)),
            2 => seed_op(OP_CALL, a, call(TOKEN, 0, None, &[&[user(next(6) as u8)][..], &small(next(200) as u16)].concat(), 0)),
            3 => {
                deal += 1;
                seed_op(OP_CALL, a, call(ESCROW, 0, Some(1 + next(9_000) as u16), &[&small(deal)[..], &[user(next(6) as u8)]].concat(), 0))
            }
            4 => seed_op(OP_CALL, a, call(ESCROW, 2, None, &small(1 + next(u64::from(deal.max(1))) as u16), 0)),
            5 => seed_op(OP_CALL, a, call(ROUTER, 1, None, &[&[program(TOKEN), user(next(6) as u8)][..], &small(next(50) as u16)].concat(), 1 << TOKEN)),
            6 => seed_op(OP_CREATE, a, [&[16, COUNTER][..], &[0], &[0x2c, 0x01]].concat()),
            // A program among those created (a selector from 128 up picks by index).
            7 => seed_op(OP_CALL, a, call(128 + 4 + next(3) as u8, 0, None, &[], 0)),
            8 => seed_op(OP_PAY, a, [&[next(4) as u8, 0][..], &(next(5_000) as u16).to_le_bytes()].concat()),
            9 => seed_op(OP_UPLOAD, 1, [&[COUNTER, 0][..], &[1, 1, next(200) as u8, 0, 1 + next(254) as u8], &[0xe8, 0x03]].concat()),
            10 => seed_op(OP_BATCH, a, [&[1u8][..], &[COUNTER, 0, 0], &[COUNTER, 0, 0], &[2, 255, 10, 0x20, 0x4e, 0, 0, 0]].concat()),
            _ => end(),
        };
        ops.push(op);
        if i % 9 == 8 {
            ops.push(end());
        }
    }
    ops
}

/// The receipts the program transactions of the v2 fixture's run of `input` ended with: the first
/// of each kind and outcome.
pub(super) fn run_for_receipts(input: &[u8]) -> BTreeMap<(&'static str, &'static str), ProgramReceipt> {
    smp_run(input, Exec::Unrolled).kept
}
