//! The overlay (docs/PROGRAMS.md 4.4): everything a transaction's programs change, kept apart
//! from the chain until the transaction succeeds. A stack of layers, one per frame: reads look
//! through the layers to the [`View`]; a frame that returns merges its layer into its caller's,
//! one that fails drops it. On success the bottom layer becomes a sorted [`Diff`], checked for
//! conservation and for who was debited before anything would apply.
//!
//! **Deposits** (6.3) are priced when a write happens, in execution order, **at the bytes once the
//! write lands**: `View::storage_price(used)`, where `used` is what `View::state_bytes()` would be
//! with the write in. That is the committed bytes, plus the net bytes this transaction's live
//! layers have added so far (removals take theirs back), plus this write's growth (its capacity
//! less the capacity of the entry it replaces); never the bytes before the write. A frame that
//! fails takes its bytes back with everything else, so a later write prices as if it never ran.
//! The node prices what it bonds outside execution (an upload's blobs, a create's program record)
//! by the same convention.
//!
//! Ordered maps only (`BTreeMap`/`BTreeSet`), so the diff's order is canonical.

use crate::abi::{flags, status};
use crate::config::{MAX_EVENT_BYTES, MAX_EVENTS, MAX_KEYS_WRITTEN, MAX_WRITTEN_BYTES};
use crate::engine::{Address, NodeFault, Trap, TrapKind};
use crate::host::View;
use crate::storage::capacity;
use std::collections::{BTreeMap, BTreeSet};

/// A stored value and the deposit behind it (6.3). The deposit is fixed when the write happens,
/// and refunded to `payer`, exactly, when the entry goes or outgrows its capacity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub value: Vec<u8>,
    pub payer: Address,
    pub deposit: u64,
    /// Accounted bytes the deposit covers (`storage::capacity` of the key and the value it was
    /// bonded for): an overwrite whose capacity is at most this bonds nothing.
    pub capacity: u32,
}

/// An event a program emitted. A claim, not a fact (8.4): the chain doesn't check its data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub program: Address,
    pub name: String,
    pub data: Vec<u8>,
}

/// One storage change: the new entry, or `None` for a removal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageChange {
    pub program: Address,
    pub key: Vec<u8>,
    pub entry: Option<Entry>,
}

/// One program's storage totals' change (6.1: its record's `items`, `bytes` and `deposit`), for
/// the node's bookkeeping. Equal to the sum, over the diff's changes to its entries, of the new
/// entry's count, capacity and deposit less the old one's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProgramStorage {
    pub program: Address,
    /// Entries added less entries removed.
    pub items: i64,
    /// Accounted bytes (entry capacities) added less removed.
    pub bytes: i64,
    /// Deposits bonded less refunded, in drops.
    pub deposit: i128,
}

/// What a successful transaction would apply, in canonical order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    /// Sorted by (program, key).
    pub storage: Vec<StorageChange>,
    /// Each program's storage totals' change, sorted by program, all-zero ones left out.
    pub programs: Vec<ProgramStorage>,
    /// The change in program state's accounted bytes: Σ `programs[i].bytes` (META's
    /// `state_bytes`, 6.2).
    pub state_bytes: i64,
    /// Net balance change per account (deposits folded in), sorted by address, zeros left out.
    pub balances: Vec<(Address, i128)>,
    /// Accounts this transaction opened, sorted.
    pub opened: Vec<Address>,
    /// Account-creation fees burned for them.
    pub burned: u128,
    /// The change in deposits held in escrow: Σ `programs[i].deposit`.
    pub escrow: i128,
}

/// Which of 4.4's commit checks a diff failed: a host bug, never a program's doing, but computed
/// from the diff alone, so every node reaches the same verdict and the transaction fails
/// (`FailCode::Commit`; the node counts it, `blockula_program_conservation_faults_total`).
/// Numbered like `TrapKind`, and as stable.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitError {
    /// A sum over the diff overflowed.
    Overflow = 1,
    /// Σ net deltas + Δ escrow + fees burned ≠ 0.
    NotConserved = 2,
    /// An account was debited that may not be.
    Debited = 3,
}

impl CommitError {
    pub fn number(self) -> u8 {
        self as u8
    }
}

impl Diff {
    /// 4.4's commit checks. **Conservation:** Σ net deltas + Δ escrow + fees burned = 0.
    /// **Who was debited:** only a program that ran in this transaction, or the signer by at most
    /// `allowance` (Σ value it sent + deposits it bonded). Conservation checks totals; this checks
    /// whose they were, so a host bug that debits the wrong account can't pass as a balanced move.
    pub fn check(&self, signer: &Address, ran: &BTreeSet<Address>, allowance: u128) -> Result<(), CommitError> {
        let burned = i128::try_from(self.burned).map_err(|_| CommitError::Overflow)?;
        let mut sum = self.escrow.checked_add(burned).ok_or(CommitError::Overflow)?;
        for (_, d) in &self.balances {
            sum = sum.checked_add(*d).ok_or(CommitError::Overflow)?;
        }
        if sum != 0 {
            return Err(CommitError::NotConserved);
        }
        for (account, d) in &self.balances {
            let ok = *d >= 0 || ran.contains(account) || (account == signer && d.unsigned_abs() <= allowance);
            if !ok {
                return Err(CommitError::Debited);
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct Layer {
    storage: BTreeMap<Address, BTreeMap<Vec<u8>, Option<Entry>>>,
    balances: BTreeMap<Address, i128>,
    opened: BTreeSet<Address>,
    burned: u128,
    escrow: i128,
    events: Vec<Event>,
    /// Keys first written in this layer (not in any below it), value bytes written, event bytes.
    keys: usize,
    written: usize,
    event_bytes: usize,
    /// Deposits the signer bonded in this layer.
    signer_bonded: u128,
    /// Accounted bytes this layer's writes and removals added (net).
    bytes: i64,
}

/// The layered overlay of one transaction.
pub struct Overlay {
    signer: Address,
    /// The transaction's own layer, under every frame's: what `finish` turns into the diff.
    bottom: Layer,
    /// One layer per running frame, the innermost last.
    frames: Vec<Layer>,
}

impl Overlay {
    /// An overlay with only the transaction's layer.
    pub fn new(signer: Address) -> Self {
        Overlay { signer, bottom: Layer::default(), frames: Vec::new() }
    }

    /// Every layer, the transaction's first and the innermost frame's last.
    fn layers(&self) -> impl DoubleEndedIterator<Item = &Layer> {
        std::iter::once(&self.bottom).chain(&self.frames)
    }

    /// A layer for a frame that's about to run.
    pub fn push(&mut self) {
        self.frames.push(Layer::default());
    }

    /// The top frame returned: its changes become its caller's.
    pub fn commit(&mut self) -> Result<(), NodeFault> {
        let child = self.frames.pop().ok_or(NodeFault::Host("an overlay commit without a frame's layer"))?;
        let parent = self.top();
        for (program, writes) in child.storage {
            parent.storage.entry(program).or_default().extend(writes);
        }
        for (account, d) in child.balances {
            *parent.balances.entry(account).or_default() += d;
        }
        parent.opened.extend(child.opened);
        parent.burned += child.burned;
        parent.escrow += child.escrow;
        parent.events.extend(child.events);
        parent.keys += child.keys;
        parent.written += child.written;
        parent.event_bytes += child.event_bytes;
        parent.signer_bonded += child.signer_bonded;
        parent.bytes += child.bytes;
        Ok(())
    }

    /// The top frame failed: everything it did is gone.
    pub fn revert(&mut self) -> Result<(), NodeFault> {
        self.frames.pop().ok_or(NodeFault::Host("an overlay revert without a frame's layer"))?;
        Ok(())
    }

    /// The innermost layer: the running frame's, or the transaction's between calls.
    fn top(&mut self) -> &mut Layer {
        self.frames.last_mut().unwrap_or(&mut self.bottom)
    }

    fn sum(&self, f: impl Fn(&Layer) -> usize) -> usize {
        self.layers().map(f).sum()
    }

    /// Deposits the signer has bonded so far (gross: refunds don't give back `storage_limit`).
    pub fn signer_bonded(&self) -> u128 {
        self.layers().map(|l| l.signer_bonded).sum()
    }

    /// Accounted bytes this transaction's live layers have added so far (net).
    fn bytes(&self) -> i64 {
        self.layers().map(|l| l.bytes).sum()
    }

    /// Program state's accounted bytes once this transaction's live layers and `change` more
    /// apply.
    fn used(&self, view: &dyn View, change: i64) -> Result<u64, NodeFault> {
        let used = i128::from(view.state_bytes()) + i128::from(self.bytes()) + i128::from(change);
        u64::try_from(used).map_err(|_| NodeFault::Host("program state's bytes out of range"))
    }

    pub fn exists(&self, view: &dyn View, account: &Address) -> bool {
        view.balance(account).is_some() || self.layers().any(|l| l.opened.contains(account))
    }

    /// `account`'s balance with this transaction's changes (0 for an account that doesn't exist).
    pub fn balance(&self, view: &dyn View, account: &Address) -> i128 {
        let delta: i128 = self.layers().filter_map(|l| l.balances.get(account)).sum();
        i128::from(view.balance(account).unwrap_or(0)) + delta
    }

    fn add(&mut self, account: Address, d: i128) {
        *self.top().balances.entry(account).or_default() += d;
    }

    /// Moves a call's value from its payer to the program called (8.1, 8.2).
    pub fn move_value(&mut self, view: &dyn View, from: Address, to: Address, amount: u64) -> Result<(), Trap> {
        if self.balance(view, &from) < i128::from(amount) {
            return Err(TrapKind::Insufficient.into());
        }
        self.add(from, -i128::from(amount));
        self.add(to, i128::from(amount));
        Ok(())
    }

    /// A program's `transfer_v1` from its own balance (10.2): a status, never a trap. To an
    /// account that doesn't exist it fails unless `flags` says who pays for opening it.
    pub fn transfer(&mut self, view: &dyn View, from: Address, to: Address, amount: u64, flags: i32, fee: u64) -> i32 {
        let (amount, fee) = (i128::from(amount), i128::from(fee));
        let have = self.balance(view, &from);
        let opening = !self.exists(view, &to);
        let (debit, credit, burn) = if !opening {
            (amount, amount, 0)
        } else if flags & flags::OPEN_DEDUCT != 0 {
            if amount < fee {
                return status::ERR_TRANSFER;
            }
            (amount, amount - fee, fee)
        } else if flags & flags::OPEN_PAY != 0 {
            (amount + fee, amount, fee)
        } else {
            return status::ERR_TRANSFER;
        };
        if have < debit {
            return status::ERR_TRANSFER;
        }
        if opening {
            let top = self.top();
            top.opened.insert(to);
            top.burned += burn as u128;
        }
        self.add(from, -debit);
        self.add(to, credit);
        status::OK
    }

    /// `(program, key)` as this transaction sees it.
    pub fn read(&self, view: &dyn View, program: &Address, key: &[u8]) -> Option<Entry> {
        for layer in self.layers().rev() {
            if let Some(w) = layer.storage.get(program).and_then(|m| m.get(key)) {
                return w.clone();
            }
        }
        view.storage(program, key)
    }

    fn written_before(&self, program: &Address, key: &[u8]) -> bool {
        self.layers().any(|l| l.storage.get(program).is_some_and(|m| m.contains_key(key)))
    }

    /// Counts one more write of `value_len` bytes to `(program, key)` against 4.4's bounds.
    fn bound_write(&mut self, program: &Address, key: &[u8], value_len: usize) -> Result<(), Trap> {
        let new_key = !self.written_before(program, key);
        if new_key && self.sum(|l| l.keys) >= MAX_KEYS_WRITTEN {
            return Err(TrapKind::KeysWritten.into());
        }
        if self.sum(|l| l.written) + value_len > MAX_WRITTEN_BYTES {
            return Err(TrapKind::WrittenBytes.into());
        }
        let top = self.top();
        top.keys += usize::from(new_key);
        top.written += value_len;
        Ok(())
    }

    /// Writes `value` under `(program, key)`. An overwrite whose capacity fits the entry's bonds
    /// nothing. Otherwise the old payer is refunded and `payer` bonds the new capacity at the price
    /// program state's bytes would be at with this write in (`View::storage_price`, the module's
    /// convention), fixed now and never again (6.3); past the ceiling there's no price
    /// (`StateFull`). The signer's bonds count against `storage_limit`.
    pub fn write(
        &mut self,
        view: &dyn View,
        program: Address,
        key: &[u8],
        value: Vec<u8>,
        payer: Address,
        storage_limit: u64,
    ) -> Result<(), Trap> {
        let old = self.read(view, &program, key);
        let need = capacity(key.len(), value.len());
        let entry = match old {
            Some(old) if need <= old.capacity => Entry { value, ..old },
            old => {
                let grows = i64::from(need) - old.as_ref().map_or(0, |o| i64::from(o.capacity));
                let price = view.storage_price(self.used(view, grows)?).ok_or(TrapKind::StateFull)?;
                // No payer could cover a deposit past `u64`.
                let deposit = price.checked_mul(u64::from(need)).ok_or(TrapKind::Insufficient)?;
                if payer == self.signer && self.signer_bonded() + u128::from(deposit) > u128::from(storage_limit) {
                    return Err(TrapKind::StorageLimit.into());
                }
                let refund = old.as_ref().filter(|o| o.payer == payer).map_or(0, |o| o.deposit);
                if self.balance(view, &payer) + i128::from(refund) < i128::from(deposit) {
                    return Err(TrapKind::Insufficient.into());
                }
                self.bound_write(&program, key, value.len())?;
                if let Some(old) = old {
                    self.add(old.payer, i128::from(old.deposit));
                    self.top().escrow -= i128::from(old.deposit);
                }
                self.add(payer, -i128::from(deposit));
                let signer = self.signer;
                let top = self.top();
                top.escrow += i128::from(deposit);
                top.bytes += grows;
                if payer == signer {
                    top.signer_bonded += u128::from(deposit);
                }
                let entry = Entry { value, payer, deposit, capacity: need };
                top.storage.entry(program).or_default().insert(key.to_vec(), Some(entry));
                return Ok(());
            }
        };
        self.bound_write(&program, key, entry.value.len())?;
        self.top().storage.entry(program).or_default().insert(key.to_vec(), Some(entry));
        Ok(())
    }

    /// Removes `(program, key)`, refunding its deposit to whoever paid it (never to the remover).
    /// Whether there was an entry.
    pub fn remove(&mut self, view: &dyn View, program: Address, key: &[u8]) -> Result<bool, Trap> {
        let Some(old) = self.read(view, &program, key) else { return Ok(false) };
        self.bound_write(&program, key, 0)?;
        self.add(old.payer, i128::from(old.deposit));
        let top = self.top();
        top.escrow -= i128::from(old.deposit);
        top.bytes -= i64::from(old.capacity);
        top.storage.entry(program).or_default().insert(key.to_vec(), None);
        Ok(true)
    }

    pub fn emit(&mut self, event: Event) -> Result<(), Trap> {
        let bytes = event.name.len() + event.data.len();
        if self.sum(|l| l.events.len()) >= MAX_EVENTS {
            return Err(TrapKind::Events.into());
        }
        if self.sum(|l| l.event_bytes) + bytes > MAX_EVENT_BYTES {
            return Err(TrapKind::EventBytes.into());
        }
        let top = self.top();
        top.event_bytes += bytes;
        top.events.push(event);
        Ok(())
    }

    /// The transaction's changes and events, once every frame has returned. A removal of a key
    /// the view never had (written and removed in this transaction) is left out. Each program's
    /// storage totals are derived from the changes against the view, and must agree with what
    /// the layers tracked (bytes and escrow), or it's the host's fault.
    pub fn finish(self, view: &dyn View) -> Result<(Diff, Vec<Event>), NodeFault> {
        if !self.frames.is_empty() {
            return Err(NodeFault::Host("an overlay finished with a frame's layer left"));
        }
        let layer = self.bottom;
        let mut storage = Vec::new();
        let mut programs = Vec::new();
        for (program, writes) in layer.storage {
            let mut totals = ProgramStorage { program, ..ProgramStorage::default() };
            for (key, entry) in writes {
                let old = view.storage(&program, &key);
                if entry.is_none() && old.is_none() {
                    continue;
                }
                for (e, sign) in [(&old, -1), (&entry, 1)] {
                    if let Some(e) = e {
                        totals.items += sign;
                        totals.bytes += sign * i64::from(e.capacity);
                        totals.deposit += i128::from(sign) * i128::from(e.deposit);
                    }
                }
                storage.push(StorageChange { program, key, entry });
            }
            if (totals.items, totals.bytes, totals.deposit) != (0, 0, 0) {
                programs.push(totals);
            }
        }
        let state_bytes: i64 = programs.iter().map(|p| p.bytes).sum();
        let deposits: i128 = programs.iter().map(|p| p.deposit).sum();
        if state_bytes != layer.bytes || deposits != layer.escrow {
            return Err(NodeFault::Host("storage totals unlike the overlay's"));
        }
        let balances = layer.balances.into_iter().filter(|(_, d)| *d != 0).collect();
        let opened = layer.opened.into_iter().collect();
        let diff = Diff { storage, programs, state_bytes, balances, opened, burned: layer.burned, escrow: layer.escrow };
        Ok((diff, layer.events))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(n: u8) -> Address {
        [n; 32]
    }

    #[test]
    fn commit_checks_conservation_and_who_was_debited() {
        let (signer, program, stranger) = (a(1), a(2), a(3));
        let ran: BTreeSet<Address> = [program].into();
        let diff = |balances: Vec<(Address, i128)>, escrow, burned| Diff { balances, escrow, burned, ..Diff::default() };
        // Value in, a transfer out, a deposit bonded, an account opened.
        let ok = diff(vec![(signer, -1_100), (program, 500), (stranger, 400)], 100, 100);
        assert_eq!(ok.check(&signer, &ran, 1_100), Ok(()));
        // One drop minted.
        let minted = diff(vec![(signer, -1_000), (program, 1_001)], 0, 0);
        assert_eq!(minted.check(&signer, &ran, 1_000), Err(CommitError::NotConserved));
        // Balanced, but a stranger paid.
        assert_eq!(diff(vec![(stranger, -5), (program, 5)], 0, 0).check(&signer, &ran, 0), Err(CommitError::Debited));
        // Balanced, but the signer paid more than it sent and bonded.
        assert_eq!(diff(vec![(signer, -6), (program, 6)], 0, 0).check(&signer, &ran, 5), Err(CommitError::Debited));
        // Sums that overflow.
        assert_eq!(diff(vec![(program, i128::MAX)], 1, 0).check(&signer, &ran, 0), Err(CommitError::Overflow));
        assert_eq!(Diff { burned: u128::MAX, ..Diff::default() }.check(&signer, &ran, 0), Err(CommitError::Overflow));
    }

    #[test]
    fn a_dropped_layer_takes_its_writes_and_counts() {
        let (signer, program) = (a(1), a(2));
        let mut o = Overlay::new(signer);
        o.push();
        o.write(&Empty, program, b"k", vec![1], signer, 200).unwrap();
        assert_eq!(o.signer_bonded(), 128);
        o.push();
        assert_eq!(o.write(&Empty, program, b"j", vec![1; 8], signer, 200), Err(TrapKind::StorageLimit.into()));
        o.write(&Empty, program, b"j", vec![1; 8], program, 200).unwrap();
        assert_eq!(o.bytes(), 256);
        o.revert().unwrap();
        assert_eq!(o.bytes(), 128, "a dropped layer takes its bytes");
        o.commit().unwrap();
        let (diff, _) = o.finish(&Empty).unwrap();
        assert_eq!(diff.storage.len(), 1);
        assert_eq!(diff.balances, vec![(signer, -128)]);
        assert_eq!((diff.escrow, diff.state_bytes), (128, 128));
        assert_eq!(diff.programs, vec![ProgramStorage { program, items: 1, bytes: 128, deposit: 128 }]);
    }

    /// A view with every account holding 1,000 drops, no programs, no storage, and a price of
    /// one drop per byte up to 1,000 bytes of state.
    struct Empty;
    impl View for Empty {
        fn height(&self) -> u64 {
            1
        }
        fn balance(&self, _: &Address) -> Option<u64> {
            Some(1_000)
        }
        fn code_of(&self, _: &Address) -> Option<crate::engine::CodeHash> {
            None
        }
        fn version_digest(&self, _: &Address) -> Option<crate::engine::Digest> {
            None
        }
        fn method(&self, _: &Address, _: &str) -> Option<crate::host::MethodInfo> {
            None
        }
        fn storage(&self, _: &Address, _: &[u8]) -> Option<Entry> {
            None
        }
        fn state_bytes(&self) -> u64 {
            0
        }
        fn storage_price(&self, used: u64) -> Option<u64> {
            (used <= 1_000).then_some(1)
        }
        fn account_creation_fee(&self) -> u64 {
            0
        }
    }

    /// Deposits price at the bytes state holds once the write lands, layers included; the
    /// ceiling refuses a write, never an overwrite that fits or a removal.
    #[test]
    fn writes_price_in_execution_order_up_to_the_ceiling() {
        struct Curve;
        impl View for Curve {
            fn height(&self) -> u64 {
                1
            }
            fn balance(&self, _: &Address) -> Option<u64> {
                Some(1_000_000)
            }
            fn code_of(&self, _: &Address) -> Option<crate::engine::CodeHash> {
                None
            }
            fn version_digest(&self, _: &Address) -> Option<crate::engine::Digest> {
                None
            }
            fn method(&self, _: &Address, _: &str) -> Option<crate::host::MethodInfo> {
                None
            }
            fn storage(&self, _: &Address, _: &[u8]) -> Option<Entry> {
                None
            }
            fn state_bytes(&self) -> u64 {
                100
            }
            // 1 drop per byte below 256 bytes of state (an eighth of 2,048), 2 below 512, …, none
            // past 4,096.
            fn storage_price(&self, used: u64) -> Option<u64> {
                crate::storage::price(1, 2_048, used)
            }
            fn account_creation_fee(&self) -> u64 {
                0
            }
        }
        let (signer, program) = (a(1), a(2));
        let mut o = Overlay::new(signer);
        o.push();
        o.write(&Curve, program, b"a", vec![], signer, u64::MAX).unwrap(); // 100 + 128 = 228 bytes: 1 a byte
        o.write(&Curve, program, b"b", vec![], signer, u64::MAX).unwrap(); // 356: 2 a byte
        assert_eq!(o.signer_bonded(), 128 + 256);
        assert_eq!(o.write(&Curve, program, b"c", vec![0; 4_000], signer, u64::MAX), Err(TrapKind::StateFull.into()), "356 + 4,160 > 4,096");
        o.write(&Curve, program, b"a", vec![0; 31], signer, u64::MAX).unwrap(); // fits its 128: nothing bonded
        assert_eq!(o.signer_bonded(), 384);
        o.remove(&Curve, program, b"b").unwrap();
        assert_eq!(o.bytes(), 128);
        o.write(&Curve, program, b"c", vec![0; 33], signer, u64::MAX).unwrap(); // 228 + 192 = 420: 2 a byte
        assert_eq!(o.signer_bonded(), 384 + 384);
        o.commit().unwrap();
        let (diff, _) = o.finish(&Curve).unwrap();
        assert_eq!(diff.state_bytes, 320);
        assert_eq!(diff.programs, vec![ProgramStorage { program, items: 2, bytes: 320, deposit: 128 + 384 }]);
        assert_eq!(diff.balances, vec![(signer, -(128 + 384))], "b's deposit came back");
    }

}
