//! Protocol v3's randomness for programs on a real chain state (docs/RANDOMNESS.md): `Chain`
//! over a lone validator, as the rest of `exec::tests`, here with its beacon duties run each block
//! (`beacon::duties`, as node.rs runs them), so the values programs read are the ones
//! `beacon::end_epoch` derives, files and prunes.
//!
//! - **The gate**: an upload of code importing `blake3_v1`, `epoch_v1` or `beacon_v1` fails under
//!   v2 (charged, nothing stored, as any import outside the ABI) and applies from the block v3
//!   activates at.
//! - **The view**: `ProgramView::beacon` is the state's value and reveal count while the state
//!   keeps it, and none before it's derived and once `end_epoch` has pruned it.
//! - **The dice**: a roll waits for its epoch, settles on the chain's beacon value, is refunded
//!   when that value came from too few reveals (the hash-chain fallback included) and when its
//!   deadline passes.
//!
//! Four validators running the same, one of them withholding its reveal, are the simulator's
//! (`sim::programs`, `the_dice_settles_identically_on_four_validators`).

use super::*;
use crate::beacon::{self, Secrets, VALUES_KEPT};
use crate::programs::receipt::ProgramReceipt;
use crate::programs::view::ProgramView;
use crate::upgrade::{self, PREVIEW_PROTOCOL_VERSION};
use blockula_vm::host::View;

/// Blocks per epoch: enough for a commitment early in it and a reveal in its second half.
const EPOCH: u64 = 6;
const BANK: u64 = 100 * FANG;

fn dice() -> (Vec<u8>, Vec<u8>) {
    (include_bytes!("../../../../vm/tests/programs/dice.wasm").to_vec(), include_bytes!("../../../../vm/tests/programs/dice.iface").to_vec())
}

/// A dev chain of [`EPOCH`]-block epochs that runs the preview version from block `at` (v2
/// before it), and its validator's beacon secrets.
struct Rig {
    d: Dev,
    secrets: Secrets,
    /// Whether the validator takes part in the beacon. Off: no commitments, so the values are the
    /// hash chain (0 reveals) and nobody is jailed.
    duties: bool,
}

impl Rig {
    fn new(tag: &str, at: u64) -> Rig {
        let preview = ScheduledUpgrade { version: PREVIEW_PROTOCOL_VERSION, activation_height: at };
        let d = Dev::new(tag, |g| {
            g.epoch_blocks = EPOCH;
            g.upgrades = if at == 1 { vec![preview] } else { vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 1 }, preview] };
        });
        let secrets = Secrets::open_deterministic(&d.home, [7; 32]).unwrap();
        Rig { d, secrets, duties: false }
    }

    fn epoch(&self) -> u64 {
        self.d.c.state.params.epoch_of(self.d.next())
    }

    /// The next block: the validator's beacon duties (if it does them), then `txs`. Each of
    /// `txs`' fate and, for a program transaction that ran, its receipt.
    fn block(&mut self, txs: Vec<Tx>) -> Vec<(Fate, Option<ProgramReceipt>)> {
        if self.duties {
            beacon::duties(&mut self.d.c, &mut self.secrets).unwrap();
        }
        let duty = self.d.c.mempool.clone();
        let fates = self.d.block([duty.clone(), txs.clone()].concat());
        assert!(fates[..duty.len()].iter().all(|f| *f == Fate::Applied), "a beacon duty failed: {fates:?}");
        let b = self.d.c.last_block().unwrap();
        let receipt = |tx: &Tx| b.txs.iter().position(|t| t.id() == tx.id()).and_then(|i| b.programs.get(&i).cloned());
        fates[duty.len()..].iter().cloned().zip(txs.iter().map(receipt)).collect()
    }

    /// One transaction from `user`, in a block of its own.
    fn tx(&mut self, user: u8, action: Action) -> (Fate, Option<ProgramReceipt>) {
        let tx = self.d.tx(&keys(user), action);
        self.block(vec![tx]).remove(0)
    }

    /// Empty blocks until the next block is the first of `epoch`.
    fn until_epoch(&mut self, epoch: u64) {
        assert!(self.epoch() < epoch || self.d.next() == epoch * EPOCH, "epoch {epoch} already began");
        while self.d.next() < epoch * EPOCH {
            self.block(vec![]);
        }
    }

    /// The beacon the state holds for `epoch`: its value and reveals.
    fn value(&self, epoch: u64) -> Option<([u8; 32], u32)> {
        self.d.c.state.beacon.values.get(&epoch).map(|v| (v.value, v.reveals))
    }

    /// Uploads the dice, creates one whose rolls need `min_reveals` and funds its bank: the
    /// program's address.
    fn dice(&mut self, user: u8, min_reveals: u32) -> String {
        let (wasm, iface) = dice();
        let hash = digest::wasm_hash(&wasm);
        if !self.d.c.state.programs.code.contains_key(&hash) {
            assert_eq!(self.tx(user, upload(wasm.clone(), iface.clone(), 10 * FANG)).0, Fate::Applied);
        }
        let program = self.d.program(user, self.d.c.state.account(&self.d.address(user)).nonce);
        assert_eq!(self.tx(user, create(&wasm, &iface, Some(("init", min_reveals.to_le_bytes().to_vec(), 0)))).0, Fate::Applied);
        let fund = calls(vec![call(&self.d, &program, "fund", vec![], BANK)], vec![]);
        assert_eq!(self.tx(user, fund).0, Fate::Applied);
        assert_eq!(self.d.balance(&program), BANK);
        program
    }

    /// `user` stakes `stake` on `pick`: the roll's id and the epochs its event names (target,
    /// deadline).
    fn roll(&mut self, user: u8, program: &str, pick: u8, stake: u64) -> (u64, u64, u64) {
        let action = calls(vec![call(&self.d, program, "roll", vec![pick], stake)], vec![]);
        let (fate, receipt) = self.tx(user, action);
        assert_eq!(fate, Fate::Applied);
        let r = receipt.unwrap();
        let (id, event) = (u64::from_le_bytes(r.calls[0].return_data.0[..].try_into().unwrap()), &r.events[0]);
        assert_eq!((event.name.as_str(), &event.data.0[..8]), ("roll", &le(id)[..]));
        let at = |i: usize| u64::from_le_bytes(event.data.0[i..i + 8].try_into().unwrap());
        (id, at(49), at(57))
    }

    fn settle(&mut self, user: u8, program: &str, id: u64) -> (Fate, Option<ProgramReceipt>) {
        let action = calls(vec![call(&self.d, program, "settle", le(id), 0)], vec![]);
        self.tx(user, action)
    }

    /// The view a program transaction of the next block would read through.
    fn view(&self) -> ProgramView<'_> {
        ProgramView::at(&self.d.c.state, self.d.next(), "test", [], None).unwrap()
    }
}

/// The face the dice at `program` rolls for request `id` on `beacon`, from the documented
/// formula: BLAKE3("dice/roll" ‖ program ‖ id ‖ beacon), its first 8 bytes mod 6, plus 1.
fn face(program: &str, id: u64, beacon: &[u8; 32]) -> u8 {
    let preimage = [&b"dice/roll"[..], &crypto::address_bytes(program).unwrap(), &le(id), beacon].concat();
    1 + (u64::from_le_bytes(blake3::hash(&preimage).as_bytes()[..8].try_into().unwrap()) % 6) as u8
}

/// A settle's one event: its name and data.
fn event(r: &ProgramReceipt) -> (&str, &[u8]) {
    assert_eq!(r.events.len(), 1, "{:?}", r.events);
    (r.events[0].name.as_str(), &r.events[0].data.0)
}

/// PRG 10.1: "code importing a function that isn't in force at upload is refused". On a chain
/// that runs v2 until block `AT` and the preview version from it, the dice's upload fails in the
/// last v2 block and applies in the first v3 one; a v2 guest uploads on both sides.
#[test]
fn code_importing_the_randomness_functions_uploads_only_from_protocol_v3() {
    const AT: u64 = 5;
    let mut r = Rig::new("rand-gate", AT);
    let alice = keys(ALICE);
    let (wasm, iface) = dice();
    let hash = digest::wasm_hash(&wasm);
    while r.d.next() < AT - 1 {
        r.block(vec![]);
    }
    // The last v2 block: refused by upload validation's import rule, charged, nothing stored.
    assert_eq!(upgrade::version_at(&r.d.c.state, r.d.next()), PROGRAMS_VERSION);
    let txs = r.d.txs(&alice, vec![upload(wasm.clone(), iface.clone(), 10 * FANG), upload_guest("counter")]);
    let (charged, before) = (r.d.charges(&txs), r.d.balance(&alice.address()));
    let fates = r.block(txs);
    assert_eq!(fates[0].0.failed(), "invalid code: upload validation rule 5 (Import)");
    assert_eq!(fates[1].0, Fate::Applied, "a guest that imports none of them uploads under v2");
    let counter_bond = fates[1].1.as_ref().unwrap().upload.as_ref().unwrap().deposit;
    assert_eq!(r.d.balance(&alice.address()), before - charged - counter_bond, "the dice's upload cost its fee and bonded nothing");
    assert!(!r.d.c.state.programs.code.contains_key(&hash));
    assert_eq!(r.d.c.state.programs.code.len(), 1);
    // The first v3 block: the same bytes upload, and the program creates and runs.
    assert_eq!((r.d.next(), upgrade::version_at(&r.d.c.state, r.d.next())), (AT, PREVIEW_PROTOCOL_VERSION));
    let (fate, receipt) = r.tx(ALICE, upload(wasm.clone(), iface, 10 * FANG));
    assert_eq!(fate, Fate::Applied);
    assert!(receipt.unwrap().upload.unwrap().new_code);
    assert!(r.d.c.state.programs.code.contains_key(&hash));
    assert_eq!(r.tx(BOB, upload_guest("token")).0, Fate::Applied, "and v2's guests still upload under v3");
    let program = r.dice(ALICE, 0);
    let (id, target, deadline) = r.roll(BOB, &program, 3, FANG);
    assert_eq!((id, target, deadline), (0, r.d.c.state.params.epoch_of(r.d.c.height()) + 2, target + 32));
    for h in 1..=r.d.c.height() {
        assert_eq!(r.d.c.block(h).unwrap().version, if h < AT { PROGRAMS_VERSION } else { PREVIEW_PROTOCOL_VERSION }, "block {h}");
    }
}

/// `beacon_v1`'s answers come from the state through `ProgramView`: none for an epoch whose
/// value isn't derived (the next epoch's, until this epoch's last block ends), the value and its
/// reveal count while the state keeps it, and none again once `beacon::end_epoch` prunes it,
/// `VALUES_KEPT` epochs later. The window is the same at every height.
#[test]
fn the_view_reads_the_beacon_from_derivation_until_it_is_pruned() {
    let mut r = Rig::new("rand-view", 1);
    r.duties = true;
    let mut seen: Vec<[u8; 32]> = vec![r.value(0).unwrap().0];
    for epoch in 1..VALUES_KEPT + 6 {
        // In the last block of the epoch before, this epoch's value doesn't exist yet.
        while r.d.next() < epoch * EPOCH - 1 {
            r.block(vec![]);
        }
        let v = r.view();
        assert_eq!((v.epoch(), v.beacon(epoch)), (epoch - 1, None), "epoch {epoch}, before it's derived");
        assert!(v.beacon(epoch - 1).is_some());
        r.block(vec![]);
        // From the first block of the epoch on, it's the state's.
        let (value, reveals) = r.value(epoch).unwrap();
        let v = r.view();
        assert_eq!(v.epoch(), epoch);
        assert_eq!(v.beacon(epoch).map(|b| (b.value, b.reveals)), Some((value, reveals)));
        assert_eq!(v.beacon(epoch + 1), None);
        assert_eq!(v.beacon(u64::MAX), None);
        // Epoch 1 has no round (`beacon.rs`); from epoch 2 on (committed to during epoch 0,
        // revealed during 1) the validator's reveal is in.
        assert_eq!(reveals, u32::from(epoch >= 2), "epoch {epoch}");
        seen.push(value);
        // Exactly the last VALUES_KEPT epochs are kept, and each reads as it did when derived.
        let oldest = (epoch + 1).saturating_sub(VALUES_KEPT);
        for e in 0..=epoch {
            let kept = v.beacon(e).map(|b| b.value);
            assert_eq!(kept, (e >= oldest).then(|| seen[e as usize]), "epoch {e} read in epoch {epoch}");
        }
    }
    assert_eq!(VALUES_KEPT, 64, "the retention window is part of beacon_v1 (PRG 10.3)");
    assert_eq!(r.d.c.state.beacon.values.len() as u64, VALUES_KEPT);
    assert!(seen.windows(2).all(|w| w[0] != w[1]));
}

/// The dice on the chain's own beacon. With no validator taking part, the values are the hash
/// chain: a roll that asks for one reveal is refunded. With the validator's reveals in, a roll
/// can't settle before its epoch, then settles on that epoch's value and pays its settler the
/// bounty; a dice that asks for more reveals than the chain has validators refunds; a roll left
/// past its deadline still settles on its value while the state keeps it; and a roll nobody
/// settled in those 64 epochs is forfeit to the bank: its player gets nothing back.
#[test]
fn the_dice_settles_refunds_and_forfeits_on_the_chains_beacon() {
    let mut r = Rig::new("rand-dice", 1);
    let one = r.dice(ALICE, 1);
    let two = r.dice(ALICE, 2);
    let (bob, carol) = (r.d.address(BOB), r.d.address(CAROL));
    let (bob_bytes, carol_bytes) = (crypto::address_bytes(&bob).unwrap(), crypto::address_bytes(&carol).unwrap());
    const BOUNTY: u64 = FANG / 100;

    // --- No reveals: the hash-chain fallback. ---
    r.until_epoch(1);
    let (id, target, _) = r.roll(BOB, &one, 4, FANG);
    assert_eq!((id, target), (0, 3));
    r.until_epoch(3);
    let (value, reveals) = r.value(3).unwrap();
    assert_eq!(reveals, 0, "nobody committed: the value is BLAKE3 over the last one and the epoch");
    assert_eq!(value, beacon::derive(&r.value(2).unwrap().0, 3, &[]), "which anyone could compute ahead");
    let (fate, receipt) = r.settle(CAROL, &one, id);
    assert_eq!(fate, Fate::Applied);
    let receipt = receipt.unwrap();
    assert_eq!(receipt.calls[0].return_data.0, [0], "no face");
    assert_eq!(event(&receipt), ("refunded", &[&le(id)[..], &bob_bytes, &le(FANG)].concat()[..]));
    assert_eq!(r.d.balance(&one), BANK, "the whole stake went back, and no bounty was paid");

    // --- The validator takes part from epoch 3: epoch 5's value is the first with its reveal. ---
    r.duties = true;
    let (id, target, deadline) = r.roll(BOB, &one, 4, FANG);
    assert_eq!((id, target, deadline), (1, 5, 37));
    let (id2, target2, _) = r.roll(BOB, &two, 4, FANG);
    assert_eq!((id2, target2), (0, 5));
    // Not in the roll's epoch, nor in the next, where the reveal lands on-chain.
    for epoch in [3, 4] {
        if r.epoch() < epoch {
            r.until_epoch(epoch);
        }
        while r.epoch() == epoch {
            let before = r.d.balance(&one);
            let (fate, _) = r.settle(CAROL, &one, id);
            assert!(fate.failed().ends_with("aborted with code 35"), "{fate:?}");
            assert_eq!(r.d.balance(&one), before, "a settle that's too early changes nothing");
        }
    }
    assert_eq!(r.epoch(), 5);
    let (value, reveals) = r.value(5).unwrap();
    assert_eq!(reveals, 1);
    let rolled = face(&one, id, &value);
    let before = r.d.balance(&one);
    let (fate, receipt) = r.settle(CAROL, &one, id);
    assert_eq!(fate, Fate::Applied);
    let receipt = receipt.unwrap();
    let payout = if rolled == 4 { 6 * FANG } else { 0 };
    assert_eq!(receipt.calls[0].return_data.0, [rolled]);
    assert_eq!(event(&receipt), ("settled", &[&le(id)[..], &bob_bytes, &[rolled], &le(payout), &carol_bytes, &le(BOUNTY)].concat()[..]));
    assert_eq!(r.d.balance(&one), before - payout - BOUNTY, "carol, who settled it, got the bounty");
    assert!(r.settle(CAROL, &one, id).0.failed().ends_with("aborted with code 34"), "settled once");
    // The same value, one reveal: under the other dice's floor of two, so its roll is refunded.
    let (fate, receipt) = r.settle(CAROL, &two, id2);
    assert_eq!(fate, Fate::Applied);
    assert_eq!(event(&receipt.unwrap()), ("refunded", &[&le(id2)[..], &bob_bytes, &le(FANG)].concat()[..]));
    assert_eq!(r.d.balance(&two), BANK);

    // --- Two rolls left alone past their deadline. ---
    let (late, target, deadline) = r.roll(BOB, &one, 2, FANG);
    assert_eq!((target, deadline), (7, 39));
    let (stale, stale_target, _) = r.roll(BOB, &one, 2, FANG);
    assert_eq!(stale_target, target);
    // Past the deadline the value is still in the state, so the first settles on it like any
    // roll: waiting gave bob nothing, and carol, a stranger, is paid to close it.
    r.until_epoch(deadline + 1);
    assert_eq!(r.epoch(), deadline + 1);
    let (value, reveals) = r.value(target).unwrap();
    assert_eq!(reveals, 1, "the value is still in the state, with its reveal");
    let rolled = face(&one, late, &value);
    let (bank, bob_before) = (r.d.balance(&one), r.d.balance(&bob));
    let (fate, receipt) = r.settle(CAROL, &one, late);
    assert_eq!(fate, Fate::Applied);
    let receipt = receipt.unwrap();
    let payout = if rolled == 2 { 6 * FANG } else { 0 };
    assert_eq!(receipt.calls[0].return_data.0, [rolled]);
    assert_eq!(event(&receipt), ("settled", &[&le(late)[..], &bob_bytes, &[rolled], &le(payout), &carol_bytes, &le(BOUNTY)].concat()[..]));
    assert_eq!(r.d.balance(&one), bank - payout - BOUNTY);
    assert!(r.d.balance(&bob) - bob_before - payout < FANG / 2, "no refund: at most the roll's deposit");
    // The value's last epoch is target + 63. From the next, nobody can tell what the second
    // roll was: forfeit. The stake stays in the bank, less the bounty for closing it.
    r.until_epoch(target + VALUES_KEPT - 1);
    assert!(r.value(target).is_some(), "kept through epoch {}", target + VALUES_KEPT - 1);
    r.until_epoch(target + VALUES_KEPT);
    assert_eq!(r.value(target), None, "the state has dropped it");
    let (bank, bob_before) = (r.d.balance(&one), r.d.balance(&bob));
    let (fate, receipt) = r.settle(CAROL, &one, stale);
    assert_eq!(fate, Fate::Applied);
    let receipt = receipt.unwrap();
    assert_eq!(receipt.calls[0].return_data.0, [0]);
    assert_eq!(event(&receipt), ("forfeited", &[&le(stale)[..], &bob_bytes, &le(FANG), &carol_bytes, &le(BOUNTY)].concat()[..]));
    assert_eq!(r.d.balance(&one), bank - BOUNTY, "the stake stayed");
    assert!(r.d.balance(&bob) - bob_before < FANG / 2, "no refund: at most the roll's deposit");
    assert!(r.settle(BOB, &one, stale).0.failed().ends_with("aborted with code 34"), "closed once");
    assert_eq!(r.d.stored(&one, b"o"), Some(le(0)), "nothing is owed once every roll is closed");
    assert!(r.d.c.state.staking.pools.values().all(|p| p.jail.is_none()), "the validator revealed every time");
}
