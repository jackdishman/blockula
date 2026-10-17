//! Beacon tests: hash vectors, a full round with every way a tx can be wrong, unbiasability
//! (reordering, placement, proposer and block hash don't matter; withholding is the one bit),
//! the hash-chain fallback, pruning, the validator's duties across a restart, revert quorums and
//! the seeded leader order.

use super::*;
use crate::chain::{now_ms, BlockInfo, Outcome, SummonOpts};
use crate::crypto::{self, Keys};
use crate::poc::{PocTimes, SOUL};

/// A chain of `n` validators (10-block epochs), opened on the first one's home (whose key is the
/// admin). Txs from the others are signed with their keys and executed by this node.
struct Net {
    root: PathBuf,
    homes: Vec<PathBuf>,
    c: Chain,
    vals: Vec<Keys>,
}

fn net(tag: &str, n: usize, contribution: Option<PocTimes>) -> Net {
    let root = std::env::temp_dir().join(format!("blockula-beacon-{tag}-{}-{}", now_ms(), std::process::id()));
    let homes = Chain::summon(&root, &SummonOpts { epoch_blocks: 10, contribution, ..SummonOpts::new("beacon", n) }).unwrap();
    let vals = homes.iter().map(|h| Keys::from_seed(hex::decode(std::fs::read_to_string(h.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap())).collect();
    let c = Chain::open(&homes[0]).unwrap();
    Net { root, homes, c, vals }
}

fn signed(c: &Chain, who: &Keys, action: Action) -> Tx {
    let from = who.address();
    let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), nonce: c.next_nonce(&from), from, fee: 0, tip: 0, valid_until: c.height() + 50, action, memo: String::new(), public_key: hex::encode(who.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
    tx.fee = min_fee(tx.size());
    tx.sign(who)
}

impl Net {
    /// Submits a tx (`Err`: the mempool's dry run refused it) and runs it in the next block.
    fn tx(&mut self, who: &Keys, action: Action) -> Result<()> {
        let tx = signed(&self.c, who, action);
        self.c.submit(tx)?;
        self.c.produce_block()
    }
    fn refused(&mut self, who: &Keys, action: Action) -> String {
        let tx = signed(&self.c, who, action);
        self.c.submit(tx).unwrap_err().to_string()
    }
    fn advance_to(&mut self, h: u64) {
        while self.c.height() < h {
            self.c.produce_block_with(vec![]).unwrap();
        }
    }
    fn done(self) {
        let root = self.c.state.root();
        drop(self.c);
        assert_eq!(Chain::open(&self.homes[0]).unwrap().state.root(), root, "a restart reaches the same state");
        std::fs::remove_dir_all(&self.root).ok();
    }
}

fn secret(i: u8) -> [u8; 32] {
    [i; 32]
}

fn commit(c: &Chain, v: &Keys, epoch: u64, s: [u8; 32]) -> Action {
    Action::BeaconCommit { epoch, commitment: hex::encode(commitment(&c.genesis.chain_id, epoch, &v.address(), &s)) }
}

fn reveal(epoch: u64, s: [u8; 32]) -> Action {
    Action::BeaconReveal { epoch, secret: hex::encode(s) }
}

/// Runs blocks `from..=to` on a copy of the state (each with its txs, a proposer and a block
/// hash), the way execution does: txs, then the end-of-block steps, then the commit.
fn world(mut st: State, chain_id: &str, from: u64, to: u64, blocks: &[(u64, Vec<Tx>)], proposer: &str, salt: &str) -> State {
    for h in from..=to {
        for (_, txs) in blocks.iter().filter(|(at, _)| *at == h) {
            for tx in txs {
                assert_eq!(st.apply(tx, chain_id, proposer, h).unwrap(), Outcome::Applied, "{:?}", tx.action);
            }
        }
        let hash = format!("{salt}-{h}");
        let tx_bytes = blocks.iter().filter(|(at, _)| *at == h).flat_map(|(_, txs)| txs).map(Tx::size).sum();
        st.end_block(&BlockInfo { height: h, hash: &hash, proposer, view: h, parent_view: h - 1, skipped: &[], tx_bytes });
        st.commit_block().unwrap();
        st.check_invariants().unwrap();
    }
    st
}

#[test]
fn hashes_are_domain_separated_and_stable() {
    let s = secret(7);
    let c = commitment("beacon", 2, "boo1v", &s);
    assert_ne!(c, commitment("other-chain", 2, "boo1v", &s), "bound to the chain");
    assert_ne!(c, commitment("beacon", 3, "boo1v", &s), "bound to the epoch");
    assert_ne!(c, commitment("beacon", 2, "boo1w", &s), "bound to the validator");
    assert_ne!(commitment("ab", 1, "c", &s), commitment("a", 1, "bc", &s), "fields are length-prefixed");
    let prev = [1u8; 32];
    let r = vec![("boo1a".to_string(), secret(1)), ("boo1b".to_string(), secret(2))];
    assert_ne!(derive(&prev, 5, &r), derive(&prev, 5, &r[..1]), "every reveal counts");
    assert_ne!(derive(&prev, 5, &[]), derive(&prev, 6, &[]), "the fallback still moves each epoch");
    assert_ne!(derive(&prev, 5, &[]), derive(&[2; 32], 5, &[]), "and chains the previous value");
    // Format change detectors (consensus rules: every node must derive the same values), checked
    // against an independent JS implementation (noble BLAKE3 in derive-key mode). The SDK's
    // `beaconCommitment` asserts the same commitment vector (sdk-ts/test/e2e.ts).
    assert_eq!(hex::encode(c), "04a492304d681e4905279135f0a7120f14ef2ea203f0b0dfdfa289c12f2e2f8a");
    assert_eq!(hex::encode(derive(&prev, 5, &r)), "ed4f87836e1cd56f7f119978d4c0e7decaa543e1b0e1d5a0a6f553fb95c25fa7");
    assert_eq!(hex::encode(genesis_value("blockula-1", 0)), "69e2aac3ae73a4d4e673feaa07488c61ea4ac584c63942d6f5e8f1319bb0ed7e");
}

/// A round on a 4-validator chain: commits during epoch 0 for epoch 2, reveals during epoch 1,
/// the value at the end of epoch 1. Every malformed or misplaced tx is refused.
#[test]
fn a_round_commits_reveals_and_records_who_missed() {
    let mut t = net("round", 4, None);
    let v = t.vals.iter().map(|k| k.clone_keys()).collect::<Vec<_>>();
    let chain_id = t.c.genesis.chain_id.clone();
    let outsider = Keys::from_seed([90; 32]);
    t.tx(&v[0], Action::Transfer { to: outsider.address(), amount: 10 * DROPS_PER_FANG }).unwrap();
    // Genesis fixes epoch 0; epoch 1 has no round.
    assert_eq!(value(&t.c.state, 0), Some(genesis_value(&chain_id, t.c.genesis.created_at_ms)));
    assert_eq!(value(&t.c.state, 1), None, "derived at the end of epoch 0");
    assert!(t.refused(&v[0], commit(&t.c, &v[0], 1, secret(1))).contains("commit to the beacon for epoch 2"));
    assert!(t.refused(&v[0], commit(&t.c, &v[0], 3, secret(1))).contains("commit to the beacon for epoch 2"));
    assert!(t.refused(&outsider, commit(&t.c, &outsider, 2, secret(9))).contains("only validators active in epoch 0"));
    assert!(t.refused(&v[0], reveal(1, secret(1))).contains("epoch 1 has no round"));
    assert!(t.refused(&v[0], reveal(2, secret(1))).contains("reveal their beacon secrets for epoch 1"));
    for (i, k) in v.iter().enumerate() {
        let tx = signed(&t.c, k, commit(&t.c, k, 2, secret(i as u8 + 1)));
        assert_eq!(t.c.state.describe(&tx, None), format!("{} committed to the randomness beacon for epoch 2", short(&k.address())));
        t.c.submit(tx).unwrap();
    }
    t.c.produce_block().unwrap();
    assert_eq!(t.c.state.beacon.rounds[&2].len(), 4);
    assert!(t.refused(&v[1], commit(&t.c, &v[1], 2, secret(9))).contains("already committed"));
    // Reveals during epoch 1, not before.
    assert!(t.refused(&v[0], reveal(2, secret(1))).contains("reveal their beacon secrets for epoch 1"));
    t.advance_to(9);
    assert_eq!(value(&t.c.state, 1), Some(derive(&value(&t.c.state, 0).unwrap(), 1, &[])), "no round: the hash chain");
    assert_eq!(value(&t.c.state, 2), None);
    assert!(t.refused(&v[0], reveal(2, secret(9))).contains("doesn't match"));
    assert!(t.refused(&v[1], reveal(2, secret(3))).contains("doesn't match"), "another validator's secret doesn't open this commitment");
    assert!(t.refused(&outsider, reveal(2, secret(9))).contains("only validators that committed during epoch 0"));
    for (i, k) in v.iter().enumerate().take(3) {
        t.c.submit(signed(&t.c, k, reveal(2, secret(i as u8 + 1)))).unwrap();
    }
    t.c.produce_block().unwrap();
    assert!(t.refused(&v[0], reveal(2, secret(1))).contains("already revealed"));
    // v[3] withholds. Nothing is known about epoch 2's value until the epoch ends.
    t.advance_to(18);
    assert_eq!(value(&t.c.state, 2), None);
    t.advance_to(19);
    let mut reveals: Vec<(String, [u8; 32])> = v.iter().take(3).enumerate().map(|(i, k)| (k.address(), secret(i as u8 + 1))).collect();
    reveals.sort();
    let got = t.c.state.beacon.values[&2].clone();
    assert_eq!(got, Value { value: derive(&value(&t.c.state, 1).unwrap(), 2, &reveals), reveals: 3, missed: vec![v[3].address()] });
    assert!(!t.c.state.beacon.rounds.contains_key(&2), "the round is gone once derived");
    // The withholder is jailed by the real `staking::jail` and slashed; nobody else is. The
    // beacon step runs first at the boundary, so the set for epoch 2 (the epoch whose value it
    // biased) is selected without it (audit F21: it used to be selected first, so the jail
    // never took a seat away), and `Unjail` waits `jail_blocks`.
    let withholder = v[3].address();
    let pool = t.c.state.staking.pools[&withholder].clone();
    let jail = pool.jail.clone().expect("jailed for withholding");
    assert_eq!((jail.at, jail.until), (19, 19 + t.c.state.chain_params.jail_blocks));
    assert!(jail.reason.contains("didn't reveal"), "{jail:?}");
    assert!(pool.slashed > 0 && pool.slashed == pool.slashed.min(crate::staking::self_bond(&t.c.state, &withholder) / 100), "{} drops", pool.slashed);
    assert!(v.iter().take(3).all(|k| t.c.state.staking.pools[&k.address()].jail.is_none()));
    assert!(t.c.state.active_sets[&1].contains(&withholder) && !t.c.state.active_sets[&2].contains(&withholder), "out of the next set");
    assert!(v.iter().take(3).all(|k| t.c.state.active_sets[&2].contains(&k.address())));
    // A repeat withholder pays more: the slash grows with its misses on record.
    let base = t.c.state.chain_params.beacon_miss_slash_bps;
    assert_eq!((miss_slash_bps(&t.c.state, &withholder), miss_slash_bps(&t.c.state, &v[0].address())), (2 * base, base));
    // A jailed validator still in the current set can't commit to a new round.
    {
        let mut st = t.c.state.clone();
        let mut j = crate::chain::Journal::default();
        let me = v[0].address();
        crate::staking::jail(&mut st, &mut j, &me, 20, 0, "downtime");
        let tx = signed(&t.c, &v[0], commit(&t.c, &v[0], 4, secret(1)));
        assert!(matches!(st.apply(&tx, &chain_id, &me, 20).unwrap(), Outcome::Failed(r) if r.contains("is jailed")));
    }
    // Idempotent for a validator that's already out: jailing it again (another miss) keeps it
    // jailed with the later release and the books balanced; a tombstoned one is left alone.
    {
        let mut st = t.c.state.clone();
        let mut j = crate::chain::Journal::default();
        let slash = st.chain_params.beacon_miss_slash_bps;
        crate::staking::jail(&mut st, &mut j, &withholder, 29, slash, "missed again");
        let again = st.staking.pools[&withholder].jail.clone().unwrap();
        assert_eq!((again.at, again.until), (29, 29 + st.chain_params.jail_blocks));
        st.check_invariants().unwrap();
        let tomb = v[2].address();
        st.staking.pools.get_mut(&tomb).unwrap().tombstoned_at = Some(1);
        let before = st.staking.pools[&tomb].clone();
        crate::staking::jail(&mut st, &mut j, &tomb, 29, 1_000, "tombstoned");
        assert_eq!(st.staking.pools[&tomb], before);
    }
    // Too late to reveal now.
    assert!(t.refused(&v[3], reveal(2, secret(4))).contains("reveal their beacon secrets for epoch 3"));
    t.done();
}

/// What a proposer (or anyone) can do within the rules doesn't move the value: the order of
/// reveals in a block, which blocks of the epoch they land in, the proposer and the block hashes.
/// The only lever is withholding a reveal, which changes it (one bit) and is recorded and jailed.
#[test]
fn only_withholding_changes_the_value() {
    let mut t = net("bias", 4, None);
    let v = t.vals.iter().map(|k| k.clone_keys()).collect::<Vec<_>>();
    let chain_id = t.c.genesis.chain_id.clone();
    for (i, k) in v.iter().enumerate() {
        t.c.submit(signed(&t.c, k, commit(&t.c, k, 2, secret(i as u8 + 1)))).unwrap();
    }
    t.c.produce_block().unwrap();
    t.advance_to(9);
    let start = t.c.state.clone();
    let reveals: Vec<Tx> = v.iter().enumerate().map(|(i, k)| signed(&t.c, k, reveal(2, secret(i as u8 + 1)))).collect();
    let rev = |i: usize| reveals[i].clone();
    let me = t.c.me().unwrap();
    let other = v[2].address();
    let a = world(start.clone(), &chain_id, 10, 19, &[(10, reveals.clone())], &me, "a");
    let b = world(start.clone(), &chain_id, 10, 19, &[(10, reveals.iter().rev().cloned().collect())], &other, "b");
    let c = world(start.clone(), &chain_id, 10, 19, &[(12, vec![rev(3)]), (15, vec![rev(2), rev(1)]), (19, vec![rev(0)])], &me, "c");
    assert_eq!(a.beacon.values[&2], b.beacon.values[&2], "order within a block, proposer and block hashes don't matter");
    assert_eq!(a.beacon.values[&2], c.beacon.values[&2], "nor which blocks the reveals land in");
    assert_eq!(a.beacon.values[&2].reveals, 4);
    // Withholding is the one choice, and it's on the record.
    let w = world(start, &chain_id, 10, 19, &[(10, reveals[..3].to_vec())], &me, "w");
    let got = &w.beacon.values[&2];
    assert_ne!(got.value, a.beacon.values[&2].value);
    assert_eq!((got.reveals, got.missed.clone()), (3, vec![v[3].address()]));
    std::fs::remove_dir_all(&t.root).ok();
}

/// Nobody takes part: each value is the hash chain's next link and nobody is jailed.
#[test]
fn without_reveals_the_beacon_is_a_hash_chain() {
    let mut t = net("chain", 1, None);
    t.advance_to(39);
    for e in 1..=4 {
        let got = &t.c.state.beacon.values[&e];
        assert_eq!(got, &Value { value: derive(&value(&t.c.state, e - 1).unwrap(), e, &[]), reveals: 0, missed: vec![] });
    }
    t.done();
}

#[test]
fn old_values_are_pruned() {
    let mut st = State::default();
    init_genesis(&mut st, "prune", 0);
    for e in 0..200 {
        end_epoch(&mut st, e);
        check_invariants(&st).unwrap();
    }
    assert_eq!(st.beacon.values.len() as u64, VALUES_KEPT);
    assert_eq!(st.beacon.values.keys().next(), Some(&(201 - VALUES_KEPT)));
    assert!(value(&st, 200).is_some() && value(&st, 100).is_none());
}

/// The validator side: `duties` every block commits and reveals on its own; secrets are on disk
/// (0600) before their commitments are sent and survive a restart; a lost secret means a missed
/// reveal (and jail).
#[test]
fn duties_commit_reveal_and_survive_a_restart() {
    let mut t = net("duties", 1, None);
    let home = t.homes[0].clone();
    let me = t.c.me().unwrap();
    let mut secrets = Secrets::open(&home).unwrap();
    fn step(t: &mut Net, secrets: &mut Secrets) {
        duties(&mut t.c, secrets).unwrap();
        t.c.produce_block().unwrap();
    }
    step(&mut t, &mut secrets);
    assert!(t.c.state.beacon.rounds[&2].contains_key(&me), "committed during epoch 0 for epoch 2");
    let path = home.join(SECRETS_FILE);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let s2 = secrets.get(2).unwrap();
    // Nothing is sent twice.
    assert!(duties(&mut t.c, &mut secrets).unwrap().is_empty());
    while t.c.height() < 8 {
        step(&mut t, &mut secrets);
    }
    assert!(t.c.mempool.is_empty());
    // A restart: the chain and the secrets come back from disk; epoch 1 reveals what epoch 0 committed.
    drop(secrets);
    drop(t.c);
    t.c = Chain::open(&home).unwrap();
    let mut secrets = Secrets::open(&home).unwrap();
    assert_eq!(secrets.get(2), Some(s2));
    // A crash between writing a secret and sending its commitment: the same secret is committed.
    t.advance_to(9);
    let s3 = secrets.get_or_create(3).unwrap();
    // The reveal waits for the second half of the epoch (block 15 of 10–19).
    while t.c.height() < 14 {
        step(&mut t, &mut secrets);
    }
    assert_eq!(t.c.state.beacon.rounds[&2][&me].revealed_at, None);
    step(&mut t, &mut secrets);
    assert_eq!(t.c.state.beacon.rounds[&2][&me].revealed_at, Some(15));
    while t.c.height() < 19 {
        step(&mut t, &mut secrets);
    }
    assert_eq!(t.c.state.beacon.values[&2], Value { value: derive(&value(&t.c.state, 1).unwrap(), 2, &[(me.clone(), s2)]), reveals: 1, missed: vec![] });
    assert_eq!(t.c.state.beacon.rounds[&3][&me].commitment, commitment(&t.c.genesis.chain_id, 3, &me, &s3));
    assert!(secrets.get(2).is_some(), "kept through the epoch it's revealed in");
    step(&mut t, &mut secrets);
    assert_eq!((secrets.get(2), secrets.get(3).is_some(), secrets.get(4).is_some()), (None, true, true), "then pruned");
    // Losing the secrets file before a reveal: the node says so once the reveal is due, misses
    // it and is jailed for it (no commitments while jailed), and once unjailed keeps going with
    // a fresh file.
    std::fs::remove_file(&path).unwrap();
    let mut secrets = Secrets::open(&home).unwrap();
    assert!(duties(&mut t.c, &mut secrets).unwrap().is_empty(), "nothing due before the middle of the epoch");
    t.advance_to(24);
    let err = duties(&mut t.c, &mut secrets).unwrap_err().to_string();
    assert!(err.contains("can't reveal"), "{err}");
    t.advance_to(29);
    assert_eq!(t.c.state.beacon.values[&3], Value { value: derive(&value(&t.c.state, 2).unwrap(), 3, &[]), reveals: 0, missed: vec![me.clone()] });
    assert!(t.c.state.staking.pools[&me].jail.is_some());
    assert!(duties(&mut t.c, &mut secrets).unwrap().is_empty(), "a jailed validator doesn't commit");
    // Epoch 4's commitment (made during epoch 2) can't be opened either: missed and jailed again.
    t.advance_to(39);
    let until = t.c.state.staking.pools[&me].jail.as_ref().unwrap().until;
    assert!(until >= 39 + t.c.state.chain_params.jail_blocks);
    t.advance_to(until - 1);
    let v0 = t.vals[0].clone_keys();
    t.tx(&v0, Action::Unjail { validator: me.clone() }).unwrap();
    assert!(t.c.state.staking.pools[&me].jail.is_none());
    let next = t.c.state.params.epoch_of(t.c.height() + 1) + 2;
    assert_eq!(duties(&mut t.c, &mut secrets).unwrap(), vec![format!("committed to the beacon for epoch {next}")]);
    t.c.produce_block().unwrap();
    assert_eq!(t.c.state.beacon.rounds[&next][&me].commitment, commitment(&t.c.genesis.chain_id, next, &me, &secrets.get(next).unwrap()));
    t.done();
}

// ---------------------------------------------------------------------------------- reverts

/// 4 validators (quorum 3), a 20-block revert window.
fn revert_net(tag: &str) -> (Net, Keys) {
    let times = PocTimes { seance_blocks: 300, claim_maturity_blocks: 20, vesting_blocks: 100, vouch_lock_blocks: 100, claim_ttl_blocks: 500, soul_half_life_blocks: 10_000 };
    let mut t = net(tag, 4, Some(times));
    let admin = t.vals[0].clone_keys();
    let alice = Keys::from_seed([31; 32]);
    t.tx(&admin, Action::Transfer { to: alice.address(), amount: 100 * DROPS_PER_FANG }).unwrap();
    t.tx(&admin, Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
    t.tx(&admin, Action::GrantSoul { to: alice.address(), amount: 20 * SOUL, reason: "test".into() }).unwrap();
    t.tx(&alice, Action::LinkGithub { handle: "alice".into(), gist_id: "abc".into() }).unwrap();
    (t, alice)
}

impl Net {
    /// A claim by `who`, verified by 3 of the 4 validators (merge commit `ab…`).
    fn verified_claim(&mut self, who: &Keys, pr: u64) -> u64 {
        self.tx(who, Action::SubmitClaim { repo: "jackdishman/blockula".into(), pr, familiar_key: None }).unwrap();
        let id = self.c.state.poc.next_claim_id - 1;
        for v in &self.vals[..3] {
            self.c.submit(signed(&self.c, v, Action::AttestClaim { claim_id: id, ok: true, merge_sha: "ab".repeat(20), note: String::new() })).unwrap();
        }
        self.c.produce_block().unwrap();
        assert_eq!(self.c.state.poc.claims[&id].status, ClaimStatus::Verified);
        id
    }
}

fn attest(id: u64, sha: &str) -> Action {
    Action::AttestRevert { claim_id: id, revert_sha: sha.into() }
}

#[test]
fn a_quorum_on_one_revert_rejects_the_claim() {
    let (mut t, alice) = revert_net("revert");
    let v: Vec<Keys> = t.vals.iter().map(|k| k.clone_keys()).collect();
    let (a, b) = ("cd".repeat(20), "ef".repeat(20));
    t.tx(&alice, Action::SubmitClaim { repo: "jackdishman/blockula".into(), pr: 1, familiar_key: None }).unwrap();
    let pending = t.c.state.poc.next_claim_id - 1;
    assert!(t.refused(&v[1], attest(pending, &a)).contains("isn't in its revert window"), "only verified claims");
    assert!(t.refused(&v[1], attest(99, &a)).contains("no claim #99"));
    let id = t.verified_claim(&alice, 2);
    assert!(t.refused(&alice, attest(id, &a)).contains("only active validators"));
    assert!(t.refused(&v[1], attest(id, &"ab".repeat(20))).contains("own merge commit"));
    // An attestation from a validator outside the current set is on record but doesn't count.
    let gone = Keys::from_seed([77; 32]).address();
    t.c.state.beacon.reverts.insert(id, BTreeMap::from([(gone, RevertAttestation { revert_sha: a.clone(), at: t.c.height() })]));
    let tx = signed(&t.c, &v[1], attest(id, &a));
    assert!(t.c.state.describe(&tx, None).ends_with(&format!("attested that claim #{id} (jackdishman/blockula#2) was reverted (commit cdcdcdcdcd)")));
    t.c.submit(tx).unwrap();
    t.c.produce_block().unwrap();
    assert!(t.refused(&v[1], attest(id, &a)).contains("already attested"));
    // Two on `a`, one on `b`: no quorum on either.
    t.tx(&v[2], attest(id, &a)).unwrap();
    t.tx(&v[3], attest(id, &b)).unwrap();
    assert_eq!(t.c.state.poc.claims[&id].status, ClaimStatus::Verified);
    assert_eq!(t.c.state.beacon.reverts[&id].len(), 4);
    let pool = t.c.state.account(&t.c.state.pool()).balance;
    let bond = t.c.state.poc.claims[&id].bond;
    t.tx(&v[0], attest(id, &a)).unwrap();
    let c = &t.c.state.poc.claims[&id];
    assert_eq!(c.status, ClaimStatus::Rejected);
    assert!(c.reason.as_deref().unwrap().contains("reverted by cdcdcdcdcd (3 of 4 validators attested)"), "{:?}", c.reason);
    assert_eq!((c.bond, t.c.state.account(&t.c.state.pool()).balance), (0, pool + bond), "the bond is slashed into the pool");
    assert!(!t.c.state.beacon.reverts.contains_key(&id));
    assert!(t.c.state.poc.claim_by_pr.contains_key("jackdishman/blockula#2"), "a reverted PR can't be claimed again");
    assert!(t.refused(&v[1], attest(id, &a)).contains("isn't in its revert window"));
    t.done();
}

#[test]
fn the_revert_window_closes_after_maturity() {
    let (mut t, alice) = revert_net("window");
    let v: Vec<Keys> = t.vals.iter().map(|k| k.clone_keys()).collect();
    let id = t.verified_claim(&alice, 3);
    let until = t.c.state.poc.claims[&id].verified_at.unwrap() + 20;
    t.tx(&v[1], attest(id, &"cd".repeat(20))).unwrap();
    t.advance_to(until - 1);
    t.tx(&v[2], attest(id, &"cd".repeat(20))).unwrap();
    assert_eq!(t.c.height(), until, "still open on its last block");
    assert!(t.refused(&v[3], attest(id, &"cd".repeat(20))).contains("isn't in its revert window"));
    assert!(t.c.state.beacon.reverts.contains_key(&id));
    // The next epoch boundary drops what can no longer reach a quorum.
    let boundary = t.c.state.params.last_of(t.c.state.params.epoch_of(t.c.height()));
    t.advance_to(boundary);
    assert!(!t.c.state.beacon.reverts.contains_key(&id));
    assert_eq!(t.c.state.poc.claims[&id].status, ClaimStatus::Verified);
    t.done();
}

// ------------------------------------------------------------------------------ leader order

/// SF-1: with a beacon value, each epoch's leaders are a permutation of the set seeded from it
/// (still every validator once per `n` views); without one, the plain rotation.
#[test]
fn leader_order_is_seeded_per_epoch() {
    let mut st = State::default();
    let keys: Vec<Keys> = (1..=5u8).map(|i| Keys::from_seed([i; 32])).collect();
    for k in &keys {
        st.validators.insert(k.address(), crate::chain::ValidatorRecord { address: k.address(), owner: k.address(), consensus_key: hex::encode(k.public_key()), p2p_addr: String::new(), http_addr: String::new(), status: crate::chain::ValidatorStatus::Active, registered_at: 0, approved_at: None, approved_by: None, active_from: Some(0), removed_at: None, removed_by: None, active_until: None, commission_bps: 0 });
    }
    let set: Vec<String> = keys.iter().map(|k| k.address()).collect();
    let sorted = crate::consensus::keys_of(keys.iter().map(|k| hex::encode(k.public_key())).collect::<Vec<_>>().iter().map(String::as_str)).unwrap();
    let rotation = |e: u64, v: u64| sorted[((e + v) % 5) as usize].address("boo");
    let mut shuffled = 0;
    for e in 0..8u64 {
        st.active_sets.insert(e, set.clone());
        assert_eq!(crate::consensus::leader(&st, e, 3), Some(rotation(e, 3)), "no beacon value: round robin");
        st.beacon.values.insert(e, Value { value: blake3::hash(&e.to_le_bytes()).into(), reveals: 1, missed: vec![] });
        let order: Vec<String> = (1..=5).map(|view| crate::consensus::leader(&st, e, view).unwrap()).collect();
        let mut each = order.clone();
        each.sort();
        let mut all = set.clone();
        all.sort();
        assert_eq!(each, all, "every validator leads once per 5 views");
        assert_eq!(crate::consensus::leader(&st, e, 6), Some(order[0].clone()), "and the order repeats");
        if order != (1..=5).map(|view| rotation(e, view)).collect::<Vec<_>>() {
            shuffled += 1;
        }
    }
    assert!(shuffled >= 6, "seeded orders differ from the rotation ({shuffled} of 8 epochs)");
    assert_eq!(crate::consensus::leader(&st, 99, 1), None, "no set, no leader");
}

/// Audit F32/F56: a validator's duties carry no tip, so a mempool full of tipping txs used to
/// refuse its beacon commitment and reveal ("mempool is full"), any 1-drop tip evicted them,
/// and proposals ranked them last: a congested half epoch jailed and slashed every validator.
/// Duties now queue in a lane of their own (outside `MAX_MEMPOOL`, never evicted for a tip) and
/// proposals take them first.
#[test]
fn a_full_mempool_doesnt_starve_the_validators_duties() {
    use crate::chain::{Account, MAX_MEMPOOL, MAX_PENDING_PER_SENDER};
    let mut t = net("full", 1, None);
    let home = t.homes[0].clone();
    let me = t.c.me().unwrap();
    let my_key = hex::encode(t.vals[0].public_key());
    let mut secrets = Secrets::open(&home).unwrap();
    // 80 accounts tip 1 drop a tx; the state machine doesn't check signatures (admission over
    // HTTP and gossip does), so these carry dummy ones.
    let senders: Vec<Keys> = (0..80u32).map(|i| Keys::from_seed(crypto::hash(&i.to_le_bytes()))).collect();
    for k in &senders {
        t.c.state.accounts.insert(k.address(), Account { balance: 1_000 * DROPS_PER_FANG, ..Default::default() });
        t.c.state.supply += 1_000 * DROPS_PER_FANG;
    }
    let pay = |c: &Chain, k: &Keys, nonce: u64, tip: u64| {
        let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), from: k.address(), nonce, fee: 0, tip, valid_until: c.height() + 100, action: Action::Transfer { to: me.clone(), amount: 1 }, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        tx.fee = 2 * min_fee(tx.size()) + tip;
        tx
    };
    'fill: for k in &senders[..79] {
        for n in 0..MAX_PENDING_PER_SENDER as u64 {
            if t.c.mempool.len() >= MAX_MEMPOOL {
                break 'fill;
            }
            t.c.submit(pay(&t.c, k, n, 1)).unwrap();
        }
    }
    assert_eq!(t.c.mempool.len(), MAX_MEMPOOL);
    let is_duty = |tx: &Tx, commit: bool| tx.public_key == my_key && matches!((&tx.action, commit), (Action::BeaconCommit { .. }, true) | (Action::BeaconReveal { .. }, false));
    // The commitment for epoch 2 gets in anyway, and outbidding the cheapest tail never evicts it.
    assert_eq!(duties(&mut t.c, &mut secrets).unwrap(), vec!["committed to the beacon for epoch 2".to_string()]);
    assert_eq!(t.c.mempool.len(), MAX_MEMPOOL + 1);
    let late = &senders[79];
    for n in 0..20 {
        t.c.submit(pay(&t.c, late, n, 2)).unwrap();
    }
    assert_eq!(t.c.mempool.len(), MAX_MEMPOOL + 1, "each outbidding tx evicted a tail");
    assert!(t.c.mempool.iter().any(|tx| is_duty(tx, true)), "never the duty");
    // A full block (the head of the proposal order) carries it.
    let order = t.c.proposal_txs();
    assert!(is_duty(order[0], true), "the duty leads, though tipped txs outrank it on tips");
    let block: Vec<Tx> = order.into_iter().take(20).cloned().collect();
    t.c.produce_block_with(block).unwrap();
    assert!(t.c.state.beacon.rounds[&2].contains_key(&me), "committed during epoch 0 for epoch 2");
    // The reveal (second half of epoch 1), under the same load.
    t.advance_to(14);
    assert!(t.c.mempool.len() >= MAX_MEMPOOL - 20, "still congested: {}", t.c.mempool.len());
    for n in 20..40 {
        t.c.submit(pay(&t.c, late, n, 2)).unwrap();
    }
    assert_eq!(duties(&mut t.c, &mut secrets).unwrap(), vec!["revealed the beacon secret for epoch 2".to_string(), "committed to the beacon for epoch 3".to_string()]);
    let order = t.c.proposal_txs();
    assert!(is_duty(order[0], false) && is_duty(order[1], true), "both duties lead, in nonce order");
    let block: Vec<Tx> = order.into_iter().take(20).cloned().collect();
    t.c.produce_block_with(block).unwrap();
    assert_eq!(t.c.state.beacon.rounds[&2][&me].revealed_at, Some(15));
    assert!(t.c.state.beacon.rounds[&3].contains_key(&me));
    std::fs::remove_dir_all(&t.root).ok();
}

/// `n` accounts funded straight in the state, for a crowd of txs that tip (the state machine
/// doesn't check signatures; admission over HTTP and gossip does, so their txs carry dummy ones).
fn crowd(t: &mut Net, n: u32, salt: u32) -> Vec<Keys> {
    let senders: Vec<Keys> = (0..n).map(|i| Keys::from_seed(crypto::hash(&[salt.to_le_bytes(), i.to_le_bytes()].concat()))).collect();
    for k in &senders {
        t.c.state.accounts.insert(k.address(), crate::chain::Account { balance: 1_000 * DROPS_PER_FANG, ..Default::default() });
        t.c.state.supply += 1_000 * DROPS_PER_FANG;
    }
    senders
}

/// A 1-drop transfer to `to` from `k` at `nonce`, tipping `tip` (a dummy signature).
fn pay(c: &Chain, k: &Keys, to: &str, nonce: u64, tip: u64) -> Tx {
    let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), from: k.address(), nonce, fee: 0, tip, valid_until: c.height() + 100, action: Action::Transfer { to: to.into(), amount: 1 }, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
    tx.fee = 2 * min_fee(tx.size()) + tip;
    tx
}

/// Queues txs tipping `tip` from `senders` (each up to `MAX_PENDING_PER_SENDER`) until the
/// mempool holds `until` txs outside the duty lane, or `senders` run out.
fn fill(t: &mut Net, senders: &[Keys], to: &str, until: usize, tip: u64) {
    let mut regular = t.c.mempool.iter().filter(|tx| !t.c.in_lane(&tx.id())).count();
    'fill: for k in senders {
        for _ in t.c.pending_count(&k.address())..crate::chain::MAX_PENDING_PER_SENDER {
            if regular >= until {
                break 'fill;
            }
            let tx = pay(&t.c, k, to, t.c.next_nonce(&k.address()), tip);
            t.c.submit(tx).unwrap();
            regular += 1;
        }
    }
}

/// Review of F32/F56: a validator that committed while in the set and left it at the next
/// boundary (jailed, outranked, under-bonded) still owes its reveal, and `end_epoch` jails and
/// slashes it for a miss. The lane used to take duties only from validators in the set of this
/// epoch or the next, so a full mempool of tipped txs refused that reveal ("mempool is full"),
/// and the validator was jailed for it: F56's outcome. A reveal is a duty while its commitment
/// is unopened, whatever the set.
#[test]
fn a_validator_that_left_the_set_still_reveals_through_a_full_mempool() {
    use crate::chain::MAX_MEMPOOL;
    let mut t = net("left", 4, None);
    let home = t.homes[0].clone();
    let me = t.c.me().unwrap();
    let mut secrets = Secrets::open(&home).unwrap();
    assert_eq!(duties(&mut t.c, &mut secrets).unwrap(), vec!["committed to the beacon for epoch 2".to_string()]);
    t.c.produce_block().unwrap();
    assert!(t.c.state.beacon.rounds[&2].contains_key(&me));
    // It leaves the set at the end of epoch 0 (jailed, say, for downtime), owing epoch 2's reveal.
    let h = t.c.height();
    crate::staking::jail(&mut t.c.state, &mut crate::chain::Journal::default(), &me, h, 0, "downtime");
    t.advance_to(9);
    assert!(!t.c.state.active_sets[&1].contains(&me) && !t.c.state.active_sets.contains_key(&2), "in neither this epoch's set nor (yet) the next");
    // The reveal half of epoch 1, under a full mempool of tipped txs.
    t.advance_to(14);
    let senders = crowd(&mut t, 80, 1);
    fill(&mut t, &senders, &me, MAX_MEMPOOL, 1);
    assert_eq!(t.c.mempool.len(), MAX_MEMPOOL);
    assert_eq!(duties(&mut t.c, &mut secrets).unwrap(), vec!["revealed the beacon secret for epoch 2".to_string()]);
    let order = t.c.proposal_txs();
    assert!(matches!(order[0].action, Action::BeaconReveal { epoch: 2, .. }) && t.c.in_lane(&order[0].id()), "in the lane, leading proposals");
    let block: Vec<Tx> = order.into_iter().take(20).cloned().collect();
    t.c.produce_block_with(block).unwrap();
    assert_eq!(t.c.state.beacon.rounds[&2][&me].revealed_at, Some(15));
    t.advance_to(19);
    assert!(!t.c.state.beacon.values[&2].missed.contains(&me), "not missed, so not jailed for it");
    assert!(!t.c.state.staking.pools[&me].jail.as_ref().unwrap().reason.contains("didn't reveal"));
    std::fs::remove_dir_all(&t.root).ok();
}

/// Review of F32: the full mempool gives up only a sender's last queued tx, and never one in the
/// duty lane, so a duty at the end of its sender's queue used to make the whole queue
/// unevictable: a validator's account could pin untipped txs during congestion behind a
/// commitment. The lane is a prefix of each sender's queue now: a duty queued behind the
/// sender's other txs queues like them (and goes first, as the cheapest tail), and joins the
/// lane once nothing else is ahead of it.
#[test]
fn a_duty_doesnt_shield_its_senders_other_txs_from_eviction() {
    use crate::chain::MAX_MEMPOOL;
    let mut t = net("pin", 1, None);
    let home = t.homes[0].clone();
    let me = t.c.me().unwrap();
    assert_eq!(t.c.system_fee_payer().as_deref(), Some(me.as_str()));
    let mut secrets = Secrets::open(&home).unwrap();
    let senders = crowd(&mut t, 81, 2);
    // The validator's account queues 10 untipped transfers, then its node the commitment.
    let v0 = t.vals[0].clone_keys();
    for _ in 0..10 {
        t.c.submit(signed(&t.c, &v0, Action::Transfer { to: senders[0].address(), amount: 1 })).unwrap();
    }
    assert_eq!(duties(&mut t.c, &mut secrets).unwrap(), vec!["committed to the beacon for epoch 2".to_string()]);
    let commit = t.c.mempool.iter().find(|tx| matches!(tx.action, Action::BeaconCommit { .. })).unwrap().id();
    assert!(!t.c.in_lane(&commit), "behind the account's other txs: not in the lane");
    fill(&mut t, &senders[..80], &me, MAX_MEMPOOL, 1);
    assert_eq!(t.c.mempool.len(), MAX_MEMPOOL);
    // Eleven txs outbid the cheapest tails: the validator's account's, the commitment first.
    let late = &senders[80];
    for n in 0..11 {
        t.c.submit(pay(&t.c, late, &me, n, 2)).unwrap();
    }
    assert_eq!((t.c.mempool.len(), t.c.pending_count(&me)), (MAX_MEMPOOL, 0), "nothing of the validator's account was pinned");
    // Resubmitted with nothing ahead of it, the commitment takes a lane slot (outside the bound).
    assert_eq!(duties(&mut t.c, &mut secrets).unwrap(), vec!["committed to the beacon for epoch 2".to_string()]);
    assert_eq!(t.c.mempool.len(), MAX_MEMPOOL + 1);
    let order = t.c.proposal_txs();
    assert!(matches!(order[0].action, Action::BeaconCommit { .. }) && t.c.in_lane(&order[0].id()));
    std::fs::remove_dir_all(&t.root).ok();
}

/// Review of fix-fees (F32): proposals rank each sender's queue by its next tx, and the node's
/// loops queue more duties of a kind than the lane holds for it (up to four attestations a tick),
/// untipped. A duty behind one of those used to wait behind the whole tipped backlog, lane or
/// not. Duties past the lane's slots now move up into it once the duties ahead of them are
/// included, so they and everything behind them lead the next proposal: a block late, not a
/// backlog late.
#[test]
fn duties_past_the_lane_wait_a_block_not_the_backlog() {
    let (mut t, alice) = revert_net("past-lane");
    let me = t.c.me().unwrap();
    let mut secrets = Secrets::open(&t.homes[0]).unwrap();
    for pr in 10..13 {
        t.tx(&alice, Action::SubmitClaim { repo: "jackdishman/blockula".into(), pr, familiar_key: None }).unwrap();
    }
    let first = t.c.state.poc.next_claim_id - 3;
    // The node attests the three claims at once, as attest_claims does: one past the lane's slots.
    let mut attested = vec![];
    for id in first..first + 3 {
        attested.push(t.c.submit_system(Action::AttestClaim { claim_id: id, ok: true, merge_sha: "ab".repeat(20), note: String::new() }, "attest").unwrap().id());
    }
    assert_eq!(attested.iter().map(|id| t.c.in_lane(id)).collect::<Vec<_>>(), vec![true, true, false]);
    // A tipped backlog (more than a block takes), then the beacon commitment.
    let senders = crowd(&mut t, 20, 3);
    fill(&mut t, &senders, &me, 600, 1);
    assert_eq!(duties(&mut t.c, &mut secrets).unwrap(), vec!["committed to the beacon for epoch 2".to_string()]);
    let at = |t: &Net| t.c.proposal_txs().iter().position(|tx| matches!(tx.action, Action::BeaconCommit { .. })).unwrap();
    assert_eq!(at(&t), t.c.mempool.len() - 1, "behind the untipped attestation, last of {}", t.c.mempool.len());
    assert!(t.c.mempool.len() > 300);
    // A block of 100 from the head of the order takes the lane's two attestations; the third and
    // the commitment behind it move up into the lane and lead the next proposal.
    let block: Vec<Tx> = t.c.proposal_txs().into_iter().take(100).cloned().collect();
    t.c.produce_block_with(block).unwrap();
    assert!(t.c.in_lane(&attested[2]));
    assert_eq!(at(&t), 1, "right behind the third attestation");
    let block: Vec<Tx> = t.c.proposal_txs().into_iter().take(100).cloned().collect();
    t.c.produce_block_with(block).unwrap();
    assert!(t.c.state.beacon.rounds[&2].contains_key(&me), "committed during epoch 0, under the backlog");
    std::fs::remove_dir_all(&t.root).ok();
}

/// Audit review-r2-consensus-2: a candidate's readiness (into a set of one or two) is sent by its
/// node from the account that pays its beacon duties. Round 2's readiness was an ordinary untipped
/// `UpdateValidator`, and the lane is a prefix of each sender's queue, so a reveal queued behind
/// it was an ordinary tx too: refused or evicted under a full mempool of tipped txs, until the
/// window closed and the validator was jailed for the miss. `ValidatorReady` is a lane duty while
/// it would apply (one admitted candidate an epoch, once), so the reveal behind it stays in the
/// lane, and both lead proposals.
#[test]
fn a_readiness_queued_ahead_of_a_reveal_keeps_it_in_the_lane() {
    use crate::chain::MAX_MEMPOOL;
    let mut t = net("ready", 3, None);
    let home = t.homes[0].clone();
    let me = t.c.me().unwrap();
    let mut secrets = Secrets::open(&home).unwrap();
    // A chain without admins (mainnet's): a set of one or two takes newcomers only by admission.
    t.c.state.params.admins.clear();
    assert_eq!(duties(&mut t.c, &mut secrets).unwrap(), vec!["committed to the beacon for epoch 2".to_string()]);
    t.c.produce_block().unwrap();
    // It sells below the minimum self-bond: out at the end of epoch 0, owing epoch 2's reveal.
    let v0 = t.vals[0].clone_keys();
    let shares = t.c.state.staking.delegations[&format!("{me}/{me}")].shares;
    t.tx(&v0, Action::Undelegate { validator: me.clone(), shares: (shares * 3 / 4) as u64, amount: 0 }).unwrap();
    t.advance_to(9);
    assert!(!t.c.state.active_sets[&1].contains(&me) && t.c.state.active_sets[&1].len() == 2);
    // It bonds back (an eligible candidate), and the set's two members admit it.
    t.tx(&v0, Action::Delegate { validator: me.clone(), amount: 1_000 * DROPS_PER_FANG }).unwrap();
    for i in [1, 2] {
        let k = t.vals[i].clone_keys();
        t.tx(&k, Action::AdmitValidator { validator: me.clone(), epoch: 1 }).unwrap();
    }
    // The epoch's last block: readiness counts there, and the reveal is due.
    t.advance_to(18);
    assert!(crate::staking::should_show_up(&t.c.state, &me, 19));
    let senders = crowd(&mut t, 80, 3);
    fill(&mut t, &senders, &me, MAX_MEMPOOL, 1);
    assert_eq!(t.c.mempool.len(), MAX_MEMPOOL);
    // The readiness goes first (a tick that ran before the reveal was due, say), the reveal behind it.
    let ready = t.c.submit_system(Action::ValidatorReady { epoch: 1 }, "ready").unwrap().id();
    assert_eq!(duties(&mut t.c, &mut secrets).unwrap(), vec!["revealed the beacon secret for epoch 2".to_string()]);
    let reveal = t.c.mempool.iter().find(|tx| matches!(tx.action, Action::BeaconReveal { .. })).unwrap().id();
    assert!(t.c.in_lane(&ready) && t.c.in_lane(&reveal), "both in the lane");
    // Outbidding the cheapest tails evicts neither.
    let late = &senders[79];
    for _ in 0..20 {
        let tx = pay(&t.c, late, &me, t.c.next_nonce(&late.address()), 2);
        t.c.submit(tx).unwrap();
    }
    let queued = |t: &Net, id: &str| t.c.mempool.iter().any(|tx| tx.id() == id);
    assert!(queued(&t, &ready) && queued(&t, &reveal) && t.c.mempool.len() == MAX_MEMPOOL + 2);
    let order = t.c.proposal_txs();
    assert_eq!((order[0].id(), order[1].id()), (ready.clone(), reveal.clone()), "both lead, in nonce order");
    let block: Vec<Tx> = order.into_iter().take(20).cloned().collect();
    t.c.produce_block_with(block).unwrap();
    assert!(!t.c.state.beacon.values[&2].missed.contains(&me) && t.c.state.staking.pools[&me].jail.is_none(), "revealed in time: not jailed for a miss");
    assert!(t.c.state.active_sets[&2].contains(&me), "admitted and ready: it joins");
    // It changed nothing but its readiness record.
    assert_eq!(t.c.state.staking.pools[&me].ready_at, Some(19));
    std::fs::remove_dir_all(&t.root).ok();
}

/// Audit review-fix-staking-2: a candidate's node signals readiness by itself, but only once a
/// quorum of the set's members admitted it (their operators' call: no node admits anyone), only
/// in the epoch's last tenth, and once.
#[test]
fn duties_signal_readiness_only_once_admitted_and_in_the_window() {
    let mut t = net("admit", 3, None);
    let home = t.homes[0].clone();
    let me = t.c.me().unwrap();
    let mut secrets = Secrets::open(&home).unwrap();
    t.c.state.params.admins.clear();
    let v0 = t.vals[0].clone_keys();
    let shares = t.c.state.staking.delegations[&format!("{me}/{me}")].shares;
    t.tx(&v0, Action::Undelegate { validator: me.clone(), shares: (shares * 3 / 4) as u64, amount: 0 }).unwrap();
    t.advance_to(9);
    t.tx(&v0, Action::Delegate { validator: me.clone(), amount: 1_000 * DROPS_PER_FANG }).unwrap();
    let ready = |t: &Net| t.c.mempool.iter().filter(|tx| matches!(tx.action, Action::ValidatorReady { .. })).count();
    // Not admitted: nothing, even in the window.
    t.advance_to(18);
    assert!(duties(&mut t.c, &mut secrets).unwrap().is_empty() && ready(&t) == 0);
    t.c.produce_block_with(vec![]).unwrap();
    assert_eq!(t.c.state.active_sets[&2].len(), 2, "not admitted: it stays out");
    // Epoch 2: both members admit it; its node waits for the window, then signals once.
    for i in [1, 2] {
        let k = t.vals[i].clone_keys();
        t.tx(&k, Action::AdmitValidator { validator: me.clone(), epoch: 2 }).unwrap();
    }
    assert!(duties(&mut t.c, &mut secrets).unwrap().is_empty() && ready(&t) == 0, "before the epoch's last tenth");
    t.advance_to(28);
    assert_eq!(duties(&mut t.c, &mut secrets).unwrap(), vec!["showed up to join the validator set (epoch 2)".to_string()]);
    assert!(duties(&mut t.c, &mut secrets).unwrap().is_empty() && ready(&t) == 1, "once");
    let before = t.c.state.validators[&me].commission_bps;
    t.c.produce_block().unwrap();
    assert_eq!((t.c.state.staking.pools[&me].ready_at, t.c.state.validators[&me].commission_bps), (Some(29), before));
    assert!(t.c.state.active_sets[&3].contains(&me), "admitted and ready: it joins");
    std::fs::remove_dir_all(&t.root).ok();
}
