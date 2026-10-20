//! The overlay (docs/PROGRAMS.md 4.4): everything a transaction's programs change, kept apart
//! from the chain until the transaction succeeds. A stack of layers, one per frame: reads look
//! through the layers to the base [`View`]; a frame that returns merges its layer into its
//! caller's, one that fails drops it. On success the bottom layer becomes a sorted [`Diff`],
//! checked for conservation and for who was debited before anything would apply.
//!
//! Ordered maps only (`BTreeMap`/`BTreeSet`), so the diff's order is canonical.

use crate::abi::{flags, status};
use crate::config::{MAX_EVENT_BYTES, MAX_EVENTS, MAX_KEYS_WRITTEN, MAX_WRITTEN_BYTES};
use crate::engine::{Address, Limit, Trap};
use crate::host::View;
use std::collections::{BTreeMap, BTreeSet};

/// A stored value and the deposit behind it (6.3). The deposit is fixed when the write happens,
/// and refunded to `payer`, exactly, when the entry goes or outgrows its capacity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub value: Vec<u8>,
    pub payer: Address,
    pub deposit: u64,
    /// Bytes (key + value) the deposit covers: an overwrite that fits bonds nothing.
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

/// What a successful transaction would apply, in canonical order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    /// Sorted by (program, key).
    pub storage: Vec<StorageChange>,
    /// Net balance change per account (deposits folded in), sorted by address, zeros left out.
    pub balances: Vec<(Address, i128)>,
    /// Accounts this transaction opened, sorted.
    pub opened: Vec<Address>,
    /// Account-creation fees burned for them.
    pub burned: u128,
    /// The change in deposits held in escrow.
    pub escrow: i128,
}

impl Diff {
    /// 4.4's commit checks. **Conservation:** Σ net deltas + Δ escrow + fees burned = 0.
    /// **Who was debited:** only a program that ran in this transaction, or the signer by at most
    /// `allowance` (Σ value it sent + deposits it bonded). Conservation checks totals; this checks
    /// whose they were, so a host bug that debits the wrong account can't pass as a balanced move.
    pub fn check(&self, signer: &Address, ran: &BTreeSet<Address>, allowance: u128) -> Result<(), &'static str> {
        let mut sum = self.escrow.checked_add(i128::try_from(self.burned).map_err(|_| "burned overflows")?).ok_or("overflow")?;
        for (_, d) in &self.balances {
            sum = sum.checked_add(*d).ok_or("overflow")?;
        }
        if sum != 0 {
            return Err("FANG not conserved");
        }
        for (account, d) in &self.balances {
            let ok = *d >= 0 || ran.contains(account) || (account == signer && d.unsigned_abs() <= allowance);
            if !ok {
                return Err("an account was debited that may not be");
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
}

/// The layered overlay of one transaction.
pub struct Overlay {
    signer: Address,
    layers: Vec<Layer>,
}

impl Overlay {
    /// An overlay with its bottom layer: the transaction's own.
    pub fn new(signer: Address) -> Self {
        Overlay { signer, layers: vec![Layer::default()] }
    }

    /// A layer for a frame that's about to run.
    pub fn push(&mut self) {
        self.layers.push(Layer::default());
    }

    /// The top frame returned: its changes become its caller's.
    pub fn commit(&mut self) {
        let child = self.layers.pop().expect("a frame's layer");
        let parent = self.layers.last_mut().expect("the transaction's layer");
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
    }

    /// The top frame failed: everything it did is gone.
    pub fn revert(&mut self) {
        assert!(self.layers.len() > 1, "the transaction's layer stays");
        self.layers.pop();
    }

    fn top(&mut self) -> &mut Layer {
        self.layers.last_mut().expect("a layer")
    }

    fn sum(&self, f: impl Fn(&Layer) -> usize) -> usize {
        self.layers.iter().map(f).sum()
    }

    /// Deposits the signer has bonded so far (gross: refunds don't give back `storage_limit`).
    pub fn signer_bonded(&self) -> u128 {
        self.layers.iter().map(|l| l.signer_bonded).sum()
    }

    pub fn exists(&self, base: &dyn View, account: &Address) -> bool {
        base.balance(account).is_some() || self.layers.iter().any(|l| l.opened.contains(account))
    }

    /// `account`'s balance with this transaction's changes (0 for an account that doesn't exist).
    pub fn balance(&self, base: &dyn View, account: &Address) -> i128 {
        let delta: i128 = self.layers.iter().filter_map(|l| l.balances.get(account)).sum();
        i128::from(base.balance(account).unwrap_or(0)) + delta
    }

    fn add(&mut self, account: Address, d: i128) {
        *self.top().balances.entry(account).or_default() += d;
    }

    /// Moves a call's value from its payer to the program called (8.1, 8.2).
    pub fn move_value(&mut self, base: &dyn View, from: Address, to: Address, amount: u64) -> Result<(), Trap> {
        if self.balance(base, &from) < i128::from(amount) {
            return Err(Trap::Insufficient);
        }
        self.add(from, -i128::from(amount));
        self.add(to, i128::from(amount));
        Ok(())
    }

    /// A program's `transfer_v1` from its own balance (10.2): a status, never a trap. To an
    /// account that doesn't exist it fails unless `flags` says who pays for opening it.
    pub fn transfer(&mut self, base: &dyn View, from: Address, to: Address, amount: u64, flags: i32, fee: u64) -> i32 {
        let (amount, fee) = (i128::from(amount), i128::from(fee));
        let have = self.balance(base, &from);
        let opening = !self.exists(base, &to);
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
    pub fn read(&self, base: &dyn View, program: &Address, key: &[u8]) -> Option<Entry> {
        for layer in self.layers.iter().rev() {
            if let Some(w) = layer.storage.get(program).and_then(|m| m.get(key)) {
                return w.clone();
            }
        }
        base.storage(program, key)
    }

    fn written_before(&self, program: &Address, key: &[u8]) -> bool {
        self.layers.iter().any(|l| l.storage.get(program).is_some_and(|m| m.contains_key(key)))
    }

    /// Counts one more write of `value_len` bytes to `(program, key)` against 4.4's bounds.
    fn bound_write(&mut self, program: &Address, key: &[u8], value_len: usize) -> Result<(), Trap> {
        let new_key = !self.written_before(program, key);
        if new_key && self.sum(|l| l.keys) >= MAX_KEYS_WRITTEN {
            return Err(Trap::Limit(Limit::KeysWritten));
        }
        if self.sum(|l| l.written) + value_len > MAX_WRITTEN_BYTES {
            return Err(Trap::Limit(Limit::WrittenBytes));
        }
        let top = self.top();
        top.keys += usize::from(new_key);
        top.written += value_len;
        Ok(())
    }

    /// Writes `value` under `(program, key)`. An overwrite that fits the entry's capacity bonds
    /// nothing; otherwise the old payer is refunded and `payer` bonds `price` per byte of the new
    /// capacity, priced now and never again (6.3). The signer's bonds count against
    /// `storage_limit`.
    #[allow(clippy::too_many_arguments)]
    pub fn write(
        &mut self,
        base: &dyn View,
        program: Address,
        key: &[u8],
        value: Vec<u8>,
        payer: Address,
        price: u64,
        storage_limit: u64,
    ) -> Result<(), Trap> {
        let old = self.read(base, &program, key);
        let need = u32::try_from(key.len() + value.len()).map_err(|_| Trap::Fault("entry size".into()))?;
        let entry = match old {
            Some(old) if need <= old.capacity => Entry { value, ..old },
            old => {
                let deposit = price.checked_mul(u64::from(need)).ok_or(Trap::Insufficient)?;
                if payer == self.signer && self.signer_bonded() + u128::from(deposit) > u128::from(storage_limit) {
                    return Err(Trap::StorageLimit);
                }
                let refund = old.as_ref().filter(|o| o.payer == payer).map_or(0, |o| o.deposit);
                if self.balance(base, &payer) + i128::from(refund) < i128::from(deposit) {
                    return Err(Trap::Insufficient);
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
    pub fn remove(&mut self, base: &dyn View, program: Address, key: &[u8]) -> Result<bool, Trap> {
        let Some(old) = self.read(base, &program, key) else { return Ok(false) };
        self.bound_write(&program, key, 0)?;
        self.add(old.payer, i128::from(old.deposit));
        let top = self.top();
        top.escrow -= i128::from(old.deposit);
        top.storage.entry(program).or_default().insert(key.to_vec(), None);
        Ok(true)
    }

    pub fn emit(&mut self, event: Event) -> Result<(), Trap> {
        let bytes = event.name.len() + event.data.len();
        if self.sum(|l| l.events.len()) >= MAX_EVENTS {
            return Err(Trap::Limit(Limit::Events));
        }
        if self.sum(|l| l.event_bytes) + bytes > MAX_EVENT_BYTES {
            return Err(Trap::Limit(Limit::EventBytes));
        }
        let top = self.top();
        top.event_bytes += bytes;
        top.events.push(event);
        Ok(())
    }

    /// The transaction's changes and events, once every frame has returned. A removal of a key
    /// the base never had (written and removed in this transaction) is left out.
    pub fn finish(mut self, base: &dyn View) -> (Diff, Vec<Event>) {
        assert_eq!(self.layers.len(), 1, "every frame returned");
        let layer = self.layers.pop().expect("the transaction's layer");
        let mut storage = Vec::new();
        for (program, writes) in layer.storage {
            for (key, entry) in writes {
                if entry.is_none() && base.storage(&program, &key).is_none() {
                    continue;
                }
                storage.push(StorageChange { program, key, entry });
            }
        }
        let balances = layer.balances.into_iter().filter(|(_, d)| *d != 0).collect();
        let diff = Diff { storage, balances, opened: layer.opened.into_iter().collect(), burned: layer.burned, escrow: layer.escrow };
        (diff, layer.events)
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
        assert!(diff(vec![(signer, -1_000), (program, 1_001)], 0, 0).check(&signer, &ran, 1_000).is_err());
        // Balanced, but a stranger paid.
        assert!(diff(vec![(stranger, -5), (program, 5)], 0, 0).check(&signer, &ran, 0).is_err());
        // Balanced, but the signer paid more than it sent and bonded.
        assert!(diff(vec![(signer, -6), (program, 6)], 0, 0).check(&signer, &ran, 5).is_err());
    }

    #[test]
    fn a_dropped_layer_takes_its_writes_and_counts() {
        struct Empty;
        impl View for Empty {
            fn height(&self) -> u64 { 1 }
            fn balance(&self, _: &Address) -> Option<u64> { Some(1_000) }
            fn code_of(&self, _: &Address) -> Option<crate::engine::CodeHash> { None }
            fn storage(&self, _: &Address, _: &[u8]) -> Option<Entry> { None }
            fn storage_price(&self) -> u64 { 1 }
            fn account_creation_fee(&self) -> u64 { 0 }
        }
        let (signer, program) = (a(1), a(2));
        let mut o = Overlay::new(signer);
        o.push();
        o.write(&Empty, program, b"k", vec![1], signer, 1, 10).unwrap();
        assert_eq!(o.signer_bonded(), 2);
        o.push();
        o.write(&Empty, program, b"j", vec![1; 8], signer, 1, 10).unwrap_err();
        o.revert();
        o.commit();
        let (diff, _) = o.finish(&Empty);
        assert_eq!(diff.storage.len(), 1);
        assert_eq!(diff.balances, vec![(signer, -2)]);
        assert_eq!(diff.escrow, 2);
    }
}
