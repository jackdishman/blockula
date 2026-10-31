//! The `program_commit` target (docs/PROGRAMS-V0.md §6.1 steps 6.1–6.4, §10.4): arbitrary
//! well-formed `Diff`s through `programs::commit::apply_diff` on a small state.
//!
//! A successful program transaction's VM work comes back as a `Diff` (storage changes, each
//! program's storage totals, the net balance changes, the accounts it opened and what they burned,
//! the deposits' change), which the VM checks for conservation and for who was debited
//! (`Diff::check`) and the node writes into the state: storage, then balances, then the accounts it
//! opened, then `value_held`. The contract of the node's side, what this target holds it to: for a
//! diff built the way the VM builds one, `apply_diff` either refuses (an overdrawn account, a
//! program that isn't one, a total out of range), or it leaves a state that keeps every invariant
//! (`State::check_invariants`, `programs::check_totals`); and the journal it writes into undoes it
//! all, refused or not, down to the state root. It never panics.
//!
//! **An input is a [`Plan`]**: the fixture's state (the v2 fixture's profiles: parameters at their
//! defaults, a low value cap, exit-only), a signer, storage writes and FANG moves, and every
//! derived part of the diff is computed from them as the VM computes it (`Overlay::finish`): each
//! program's totals from its writes against the entries the state holds, the deposits as moves
//! between their payers' balances and escrow, the net balances sorted with zeros left out, the
//! accounts a move opens (the creation fee is burned, taken from the amount), so conservation holds
//! by construction (a self-check: a diff `Diff::check` refuses is the harness's bug). What the
//! fuzzer chooses is what the VM's host chooses: which programs and keys, whether a write overwrites
//! within the capacity already bonded or bonds anew, who pays, how much, and what moves where, an
//! overdraft and a program that doesn't exist among them.
//!
//! The wire form is [`Plan::decode`] (every byte string is some plan: zeros past the end) and
//! [`Plan::encode`] (its inverse, which the seeds use).

use super::oracle;
use super::v2::{self, Bytes, Profile};
use super::*;
use crate::programs::commit;
use blockula_vm::config::{MAX_KEY_BYTES, MAX_VALUE_BYTES};
use blockula_vm::exec::{Diff, Entry, ProgramStorage, StorageChange};
use blockula_vm::storage::capacity;
use std::collections::BTreeSet;

/// How a storage write sets its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Write {
    /// Takes the entry away (the key's deposit goes back to its payer).
    Remove,
    /// Overwrites an entry the state holds, keeping the payer, deposit and capacity it was bonded
    /// with, if the value fits that capacity; otherwise as `Signer` bonds (the VM's rule: an
    /// overwrite that fits bonds nothing, §6.3).
    Overwrite,
    /// Bonds a new entry paid by the signer: `deposit` drops, for the capacity its key and value
    /// need.
    Signer(u64),
    /// Bonds a new entry paid by the program itself (`SPONSOR`).
    Sponsor(u64),
}

/// One storage write: a program (a selector: one in sixteen names a program that doesn't exist, the
/// rest the guests), a key of 1–128 bytes, a value, and how it's written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Put {
    program: u8,
    key: Vec<u8>,
    value: Vec<u8>,
    write: Write,
}

/// FANG moving from the signer or a program to an account (a program, a user, the validator, a
/// newcomer, or one the state doesn't hold, which the move opens).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Move {
    /// 0 the signer; else the program it selects.
    from: u8,
    to: u8,
    amount: u64,
}

/// What `program_commit` runs: see the module docs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    profile: Profile,
    signer: u8,
    puts: Vec<Put>,
    moves: Vec<Move>,
}

/// Writes of at most this many keys, and moves of at most this many, in a plan.
const MAX_PUTS: usize = 6;
const MAX_MOVES: usize = 4;

impl Plan {
    /// The plan `data` encodes: its first byte picks the profile and the signer (a user), then the
    /// writes (a count, then each: program, key length and key, how, value length and value, and a
    /// deposit when it bonds), then the moves (a count, then each: from, to, amount).
    pub(super) fn decode(data: &[u8]) -> Plan {
        let mut rd = Bytes(data);
        let first = rd.u8();
        let profile = Profile::ALL[first as usize / SM_USERS % Profile::ALL.len()];
        let signer = first % SM_USERS as u8;
        let puts = (0..rd.u8() as usize % (MAX_PUTS + 1))
            .map(|_| {
                let program = rd.u8();
                let key_len = 1 + rd.u8() as usize % MAX_KEY_BYTES;
                let key = rd.take(key_len);
                let how = rd.u8() % 4;
                let value_len = rd.u16() as usize % (MAX_VALUE_BYTES + 1);
                let value = rd.take(value_len);
                let deposit = u64::from_le_bytes(rd.take(8).try_into().expect("8 bytes"));
                let write = match how {
                    0 => Write::Remove,
                    1 => Write::Overwrite,
                    2 => Write::Signer(deposit),
                    _ => Write::Sponsor(deposit),
                };
                Put { program, key, value, write }
            })
            .collect();
        let moves = (0..rd.u8() as usize % (MAX_MOVES + 1)).map(|_| Move { from: rd.u8(), to: rd.u8(), amount: u64::from_le_bytes(rd.take(8).try_into().expect("8 bytes")) }).collect();
        Plan { profile, signer, puts, moves }
    }

    /// The bytes [`Plan::decode`] reads back as this plan (for a plan whose parts are within the
    /// bounds `decode` keeps them to).
    pub(super) fn encode(&self) -> Vec<u8> {
        let profile = Profile::ALL.iter().position(|p| *p == self.profile).expect("a profile") as u8;
        let mut out = vec![profile * SM_USERS as u8 + self.signer, self.puts.len() as u8];
        for p in &self.puts {
            let (how, deposit) = match p.write {
                Write::Remove => (0, 0),
                Write::Overwrite => (1, 0),
                Write::Signer(d) => (2, d),
                Write::Sponsor(d) => (3, d),
            };
            out.extend([p.program, (p.key.len() - 1) as u8]);
            out.extend(&p.key);
            out.push(how);
            out.extend((p.value.len() as u16).to_le_bytes());
            out.extend(&p.value);
            out.extend(deposit.to_le_bytes());
        }
        out.push(self.moves.len() as u8);
        for m in &self.moves {
            out.extend([m.from, m.to]);
            out.extend(m.amount.to_le_bytes());
        }
        out
    }
}

/// The state a plan runs on, and what its selectors name.
struct World {
    state: State,
    /// The height the diff would commit at, and the FANG programs control before it.
    height: u64,
    signer: [u8; 32],
    /// The programs, the guests first, then a program that doesn't exist (the last entry).
    programs: Vec<[u8; 32]>,
    /// Every account a move may pay: the programs, then users, the validator, newcomers, and an
    /// address the state has no account for.
    accounts: Vec<[u8; 32]>,
}

impl World {
    fn of(plan: &Plan) -> World {
        let fx = v2::fixture(plan.profile);
        let parse = |a: &str| crate::crypto::address_bytes(a).expect("an address the fixture made parses");
        let guests = fx.programs.as_ref().expect("the v2 fixture has its guests");
        let mut programs: Vec<[u8; 32]> = guests.addresses.iter().map(|a| parse(a)).collect();
        programs.push([0xee; 32]);
        let mut accounts = programs.clone();
        accounts.extend(fx.users.iter().map(|u| parse(&u.address)));
        accounts.push(parse(&fx.validator.address));
        accounts.extend(fx.newcomers.iter().map(|a| parse(a)));
        accounts.push([0xdd; 32]);
        // The guests hold some FANG for a diff to pay out of (10 FANG the first, 20 the second…),
        // sent them from the validator's account as a transfer into a program does, so the totals
        // (`value_held`) and the supply stay as `check_invariants` has them.
        let mut state = fx.state.clone();
        let mut j = crate::chain::Journal::default();
        for (i, address) in guests.addresses.iter().enumerate() {
            let amount = (i as u64 + 1) * 10 * types::DROPS_PER_FANG;
            state.j_debit(&mut j, &fx.validator.address, amount).expect("the validator holds millions");
            state.j_credit(&mut j, address, amount);
            state.programs.meta.as_mut().expect("the fixture has programs").value_held += amount;
        }
        state.check_invariants().expect("the funded fixture state keeps its invariants");
        World { state, height: fx.height + 1, signer: parse(&fx.users[plan.signer as usize].address), programs, accounts }
    }

    /// The program a selector names: one in sixteen is the one that doesn't exist (an overdraft or
    /// a write to it is refused, which the seeds can't hold), the rest are the guests.
    fn program(&self, selector: u8) -> [u8; 32] {
        let guests = self.programs.len() - 1;
        if selector % 16 == 15 { self.programs[guests] } else { self.programs[selector as usize % guests] }
    }

    fn render(&self, a: &[u8; 32]) -> String {
        crate::crypto::render_address(self.state.params.prefix(), a)
    }

    /// The diff of `plan`, derived as `Overlay::finish` derives one (the module docs).
    fn diff(&self, plan: &Plan) -> Diff {
        // The accounts' net change, the deposits' change, and what's burned.
        let mut net: BTreeMap<[u8; 32], i128> = BTreeMap::new();
        let (mut escrow, mut burned) = (0i128, 0u128);
        let mut writes: BTreeMap<([u8; 32], Vec<u8>), Option<Entry>> = BTreeMap::new();
        for p in &plan.puts {
            let program = self.program(p.program);
            // The state's entry, or what an earlier write of this plan left (the VM merges a
            // transaction's writes to one key into one).
            let stored = |key: &Vec<u8>| self.state.programs.storage.get(&crate::programs::state::storage_key(&program, key)).map(|e| Entry { value: e.value.clone(), payer: e.payer, deposit: e.deposit, capacity: e.capacity });
            let old = stored(&p.key);
            let need = capacity(p.key.len(), p.value.len());
            let bond = |payer: [u8; 32], deposit: u64| Entry { value: p.value.clone(), payer, deposit, capacity: need };
            let entry = match (p.write, &old) {
                (Write::Remove, _) => None,
                (Write::Overwrite, Some(o)) if need <= o.capacity => Some(Entry { value: p.value.clone(), ..o.clone() }),
                (Write::Overwrite, _) => Some(bond(self.signer, 0)),
                (Write::Signer(d), _) => Some(bond(self.signer, d)),
                (Write::Sponsor(d), _) => Some(bond(program, d)),
            };
            writes.insert((program, p.key.clone()), entry);
        }
        let mut storage = vec![];
        let mut totals: BTreeMap<[u8; 32], ProgramStorage> = BTreeMap::new();
        for ((program, key), entry) in writes {
            let old = self.state.programs.storage.get(&crate::programs::state::storage_key(&program, &key)).map(|e| Entry { value: e.value.clone(), payer: e.payer, deposit: e.deposit, capacity: e.capacity });
            if entry.is_none() && old.is_none() {
                continue;
            }
            let t = totals.entry(program).or_insert(ProgramStorage { program, ..ProgramStorage::default() });
            for (e, sign) in [(&old, -1i64), (&entry, 1)] {
                if let Some(e) = e {
                    t.items += sign;
                    t.bytes += sign * i64::from(e.capacity);
                    t.deposit += i128::from(sign) * i128::from(e.deposit);
                }
            }
            // A deposit moves between its payer's balance and escrow, unless the entry keeps the
            // one it was bonded with.
            let kept = matches!((&old, &entry), (Some(o), Some(n)) if (o.payer, o.deposit, o.capacity) == (n.payer, n.deposit, n.capacity));
            if !kept {
                if let Some(o) = &old {
                    *net.entry(o.payer).or_default() += i128::from(o.deposit);
                }
                if let Some(n) = &entry {
                    *net.entry(n.payer).or_default() -= i128::from(n.deposit);
                }
            }
            if let Some(n) = &entry {
                escrow += i128::from(n.deposit);
            }
            if let Some(o) = &old {
                escrow -= i128::from(o.deposit);
            }
            storage.push(StorageChange { program, key, entry });
        }
        let programs: Vec<ProgramStorage> = totals.into_values().filter(|t| (t.items, t.bytes, t.deposit) != (0, 0, 0)).collect();
        let state_bytes = programs.iter().map(|p| p.bytes).sum();
        // The moves. One to an account the state doesn't hold opens it: the creation fee is
        // burned out of the amount, which must cover it.
        let fee = self.state.chain_params.account_creation_fee;
        let mut opened = BTreeSet::new();
        for m in &plan.moves {
            let from = if m.from == 0 { self.signer } else { self.program(m.from - 1) };
            let to = self.accounts[m.to as usize % self.accounts.len()];
            let opens = self.state.accounts.get(&self.render(&to)).is_none() && !opened.contains(&to);
            let credit = if opens {
                let Some(rest) = m.amount.checked_sub(fee) else { continue };
                opened.insert(to);
                burned += u128::from(fee);
                rest
            } else {
                m.amount
            };
            *net.entry(from).or_default() -= i128::from(m.amount);
            *net.entry(to).or_default() += i128::from(credit);
        }
        let balances = net.into_iter().filter(|(_, d)| *d != 0).collect();
        Diff { storage, programs, state_bytes, balances, opened: opened.into_iter().collect(), burned, escrow }
    }
}

/// What `apply_diff` must have written for a diff it took in, read back from the state: every
/// account's balance is its balance before plus the diff's change to it (and the supply less what
/// the diff burned), every storage change is the entry the state now holds (or none, for a
/// removal), every program's totals and META's moved by the diff's, computed here from the diff and
/// the state before it, never from `apply_diff`'s own sums.
fn check_written(before: &State, after: &State, diff: &Diff, plan: &Plan) {
    let render = |a: &[u8; 32]| crate::crypto::render_address(before.params.prefix(), a);
    let balance = |s: &State, a: &str| s.accounts.get(a).map_or(0i128, |x| i128::from(x.balance));
    // Balances: all of them.
    let delta = |a: &str| -> i128 { diff.balances.iter().filter(|(b, _)| render(b) == a).map(|(_, d)| *d).sum() };
    let names: BTreeSet<&str> = before.accounts.keys().chain(after.accounts.keys()).map(String::as_str).collect();
    for name in names {
        assert_eq!(balance(after, name), balance(before, name) + delta(name), "account {name} went from {} to {} drops through a diff that changes it by {}: plan {plan:?}\ndiff {diff:?}", balance(before, name), balance(after, name), delta(name));
    }
    for (account, d) in &diff.balances {
        assert!(after.accounts.contains_key(&render(account)) || *d == 0, "an account the diff changes isn't in the state: {}", render(account));
    }
    assert_eq!(u128::from(after.supply) + diff.burned, u128::from(before.supply), "the supply went from {} to {} through a diff that burns {}", before.supply, after.supply, diff.burned);
    // Storage: each change is what the state holds now.
    for change in &diff.storage {
        let held = after.programs.storage.get(&crate::programs::state::storage_key(&change.program, &change.key));
        let want = change.entry.as_ref();
        let got = held.map(|e| Entry { value: e.value.clone(), payer: e.payer, deposit: e.deposit, capacity: e.capacity });
        assert_eq!(got.as_ref(), want, "the entry of key {:?} of {} reads {got:?} after a diff that writes {want:?}: plan {plan:?}", change.key, render(&change.program));
    }
    // Totals: each program's, and META's.
    for p in &diff.programs {
        let (old, new) = (&before.programs.records[&render(&p.program)], &after.programs.records[&render(&p.program)]);
        assert_eq!((i128::from(new.items), i128::from(new.bytes), i128::from(new.deposit)), (i128::from(old.items) + i128::from(p.items), i128::from(old.bytes) + i128::from(p.bytes), i128::from(old.deposit) + p.deposit), "{}'s totals went from {old:?} to {new:?} through {p:?}", render(&p.program));
    }
    let (m0, m1) = (before.programs.meta.as_ref().expect("META"), after.programs.meta.as_ref().expect("META"));
    assert_eq!(i128::from(m1.state_bytes), i128::from(m0.state_bytes) + i128::from(diff.state_bytes), "program state's bytes");
    assert_eq!(i128::from(m1.escrow), i128::from(m0.escrow) + diff.escrow, "programs' escrow");
    // Sponsored: the deposits programs paid themselves, the entries now less the entries replaced.
    let sponsored: i128 = diff.storage.iter().map(|c| {
        let was = before.programs.storage.get(&crate::programs::state::storage_key(&c.program, &c.key)).filter(|e| e.payer == c.program).map_or(0i128, |e| i128::from(e.deposit));
        let now = c.entry.as_ref().filter(|e| e.payer == c.program).map_or(0i128, |e| i128::from(e.deposit));
        now - was
    }).sum();
    assert_eq!(i128::from(m1.sponsored), i128::from(m0.sponsored) + sponsored, "programs' sponsored deposits");
    let held: i128 = diff.balances.iter().filter(|(a, _)| before.programs.records.contains_key(&render(a))).map(|(_, d)| *d).sum();
    assert_eq!(i128::from(m1.value_held), i128::from(m0.value_held) + held, "the FANG programs hold");
}

/// Whether a plan reads back as itself from its bytes (`Plan::decode` of what `Plan::encode` wrote
/// of the plan `data` decodes to): for `tests/harness.rs`, which holds every seed to it.
pub fn plan_is_stable(data: &[u8]) -> bool {
    let plan = Plan::decode(data);
    Plan::decode(&plan.encode()) == plan
}

/// What became of a plan's diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Commit {
    /// `apply_diff` took it in and the control's check passed: a call's commit that stands.
    Kept,
    /// `apply_diff` took it in and the control's check refused it (the value cap, exit-only): the
    /// transaction fails and its journal is rolled back.
    Refused,
    /// `apply_diff` refused it: an overdrawn account, a program that isn't one, a total out of range.
    Rejected,
}

/// Runs the plan `data` encodes: the diff of the plan through `apply_diff` and the control's check,
/// as a call's commit runs them (`programs::call::apply`), then what it must leave. Accepted:
/// `apply_diff` took the diff in (the control's check may still have refused it, as the cap or
/// exit-only do), not an overdraft or a program that isn't one.
pub fn program_commit(data: &[u8]) -> bool {
    commit_of(data) != Commit::Rejected
}

/// [`program_commit`], saying what became of the diff (for `tests/harness.rs`).
pub fn commit_of(data: &[u8]) -> Commit {
    commit_tampered(data, None)
}

/// [`commit_of`] with `tamper` called on the state right after `apply_diff` wrote the diff into it:
/// the change a defect in `apply_diff` would make, which `tests/oracles.rs` shows the checks see.
pub fn commit_tampered(data: &[u8], tamper: Option<fn(&mut State, &Diff)>) -> Commit {
    let plan = Plan::decode(data);
    let world = World::of(&plan);
    let diff = world.diff(&plan);
    // The plan is well formed: what the VM checks of a diff it hands over.
    let ran: BTreeSet<[u8; 32]> = world.programs.iter().copied().collect();
    assert_eq!(diff.check(&world.signer, &ran, u128::MAX), Ok(()), "the harness built a diff the VM would refuse: {diff:?}");
    fn ascending<T: Ord>(v: Vec<T>) -> bool {
        v.windows(2).all(|w| w[0] < w[1])
    }
    assert!(ascending(diff.storage.iter().map(|c| (c.program, c.key.clone())).collect()) && ascending(diff.programs.iter().map(|p| p.program).collect()) && ascending(diff.balances.iter().map(|b| b.0).collect()) && ascending(diff.opened.clone()), "the harness built a diff out of order");

    let mut state = world.state.clone();
    let root = state.full_root();
    let (control, before) = (commit::control(&state), world.state.clone());
    let mut j = crate::chain::Journal::default();
    let written = commit::apply_diff(&mut state, &mut j, &diff);
    if let Some(tamper) = tamper {
        tamper(&mut state, &diff);
    }
    // A caller keeps a diff that went in only if the control's check passes too.
    let result = match &written {
        Err(_) => Commit::Rejected,
        Ok(()) if commit::check_control(&state, control, world.height).is_ok() => Commit::Kept,
        Ok(()) => Commit::Refused,
    };
    if written.is_ok() {
        check_written(&before, &state, &diff, &plan);
    }
    match result {
        Commit::Refused => {
            // Only what raises the control past where it may rise is refused (§7.1, §7.3): the
            // control rose, and either programs are exit-only or it passed the cap.
            let (after, cap) = (commit::control(&state), oracle::value_cap(&state));
            assert!(after > control && (!oracle::feature_on(&state) || after > u128::from(cap)), "a diff was refused that doesn't raise the control past the cap of {cap} (exit-only: {}): the control went from {control} to {after}: plan {plan:?}", !oracle::feature_on(&state));
        }
        Commit::Kept => {
            if let Err(e) = state.check_invariants() {
                panic!("a diff went through apply_diff and broke an invariant: {e:#}\nplan {plan:?}\ndiff {diff:?}");
            }
            assert!(crate::programs::check_totals(&state), "a diff went through apply_diff and left the program totals drifted");
            let meta = state.programs.meta.as_ref().expect("the fixture has programs");
            let after = commit::control(&state);
            assert_eq!(after, u128::from(meta.value_held) + u128::from(meta.sponsored), "the control isn't value held plus sponsored");
            // What the control's check keeps (§7.1, §7.3): a diff that stands doesn't raise the FANG
            // programs control past the value cap (a cap lowered under it only stops it rising), nor
            // at all while programs are exit-only.
            let cap = u128::from(oracle::value_cap(&state));
            assert!(after <= control.max(cap), "a diff that stands took the control from {control} to {after}, past the cap of {cap}: plan {plan:?}");
            assert!(after <= control || oracle::feature_on(&state), "a diff that stands raised the control from {control} to {after} while programs are exit-only: plan {plan:?}");
        }
        Commit::Rejected => {}
    }
    // The caller's rollback (`apply_j`'s) undoes all of it, a refusal's partial writes included.
    state.rollback(j);
    assert_eq!(state.full_root(), root, "the journal didn't undo a diff's writes ({:?}): plan {plan:?}", written.as_ref().err());
    if let Err(e) = state.check_invariants() {
        panic!("the state after a rollback breaks an invariant: {e:#}");
    }
    result
}

// ----------------------------------------------------------------------------------------- seeds

fn put(program: u8, key: &[u8], value: &[u8], write: Write) -> Put {
    Put { program, key: key.to_vec(), value: value.to_vec(), write }
}

/// The `program_commit` seeds: the shapes the guests' calls make (a first write, an overwrite, a
/// removal, a sponsored deposit funded in the same diff or paid from the program's own balance, FANG
/// into and out of a program, an account opened) and a plan on each profile. The refusals (an
/// overdraft, a program that isn't one) aren't seeds: a seed must be taken in (`tests/seeds.rs`),
/// and `tests/harness.rs` holds them to their refusal. Deposits are what the fixture's price makes
/// of the capacity (100,000 drops a byte).
pub(super) fn seeds() -> Vec<(&'static str, Vec<u8>)> {
    const COUNTER: u8 = 0;
    const TOKEN: u8 = 1;
    const ESCROW: u8 = 2;
    let price = 100_000u64;
    let deposit = |key: usize, value: usize| u64::from(capacity(key, value)) * price;
    let plan = |profile, signer, puts: Vec<Put>, moves: Vec<Move>| Plan { profile, signer, puts, moves }.encode();
    let fang = types::DROPS_PER_FANG;
    // `to` selectors: 0–4 the programs and the one that isn't, 5–10 users, 11 the validator, 12–19 newcomers, 20 a new address.
    let (user, new) = (|i: u8| 5 + i, 20u8);
    let first = put(COUNTER, b"n", &1u64.to_le_bytes(), Write::Signer(deposit(1, 8)));
    vec![
        ("counter-first-increment", plan(Profile::Default, 1, vec![first.clone()], vec![])),
        ("counter-overwrite-within-capacity", plan(Profile::Default, 1, vec![put(COUNTER, b"n", &2u64.to_le_bytes(), Write::Overwrite)], vec![])),
        ("token-two-balances", plan(Profile::Default, 0, vec![put(TOKEN, &[&[b'b'][..], &[1; 32]].concat(), &100u64.to_le_bytes(), Write::Signer(deposit(33, 8))), put(TOKEN, &[&[b'b'][..], &[2; 32]].concat(), &5u64.to_le_bytes(), Write::Signer(deposit(33, 8)))], vec![])),
        ("remove-a-key-the-state-holds", plan(Profile::Default, 0, vec![put(TOKEN, b"s", &[], Write::Remove), put(TOKEN, b"nothing here", &[], Write::Remove)], vec![])),
        ("escrow-open-with-value", plan(Profile::Default, 2, vec![put(ESCROW, &[&[b'd'][..], &[1; 8]].concat(), &[7; 72], Write::Signer(deposit(9, 72)))], vec![Move { from: 0, to: ESCROW, amount: 5 * fang }])),
        ("escrow-release-to-a-user", plan(Profile::Default, 2, vec![], vec![Move { from: ESCROW + 1, to: user(3), amount: 5 * fang }])),
        ("sponsored-deposit-funded-in-the-diff", plan(Profile::Default, 1, vec![put(COUNTER, b"sponsored", &[1, 2, 3], Write::Sponsor(deposit(9, 3)))], vec![Move { from: 0, to: COUNTER, amount: 2 * fang }])),
        ("sponsored-deposit-from-the-programs-own-balance", plan(Profile::Default, 1, vec![put(COUNTER, b"sponsored", &[1, 2, 3], Write::Sponsor(deposit(9, 3)))], vec![])),
        ("program-pays-two-users", plan(Profile::Default, 2, vec![], vec![Move { from: ESCROW + 1, to: user(3), amount: 5 * fang }, Move { from: ESCROW + 1, to: user(4), amount: 3 * fang }])),
        ("program-pays-a-new-account", plan(Profile::Default, 1, vec![], vec![Move { from: ESCROW + 1, to: new, amount: fang }])),
        ("value-past-the-tight-cap", plan(Profile::Tight, 1, vec![], vec![Move { from: 0, to: COUNTER, amount: 400 * fang }, Move { from: 0, to: ESCROW, amount: 300 * fang }])),
        ("value-into-exit-only-programs", plan(Profile::ExitOnly, 1, vec![], vec![Move { from: 0, to: COUNTER, amount: fang }])),
        ("overwrite-and-remove-the-same-key", plan(Profile::Default, 1, vec![first, put(COUNTER, b"n", &[], Write::Remove)], vec![])),
    ]
}
