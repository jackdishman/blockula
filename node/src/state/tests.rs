//! State layer tests: random tx sequences against the incremental root and the invariants,
//! proofs, commits, snapshots, restart determinism, clone cost, and a benchmark.

use super::*;
use crate::chain::{now_ms, Account, Chain, SummonOpts};
use crate::crypto::{self, Keys};
use crate::poc::{PocTimes, JUROR_BOND, SOUL};
use crate::types::*;
use proptest::prelude::*;
use std::path::PathBuf;
use std::time::Instant;

const USERS: usize = 6;
const AGENTS: usize = 3;

/// A one-validator chain (the validator is also the admin) with fast Proof of Contribution
/// timings, 10-block epochs and funded users.
struct Sim {
    home: PathBuf,
    c: Chain,
    admin: Keys,
    users: Vec<Keys>,
    agents: Vec<Keys>,
}

fn sim(tag: &str) -> Sim {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!("blockula-state-{tag}-{}-{}-{n}", now_ms(), std::process::id()));
    let times = PocTimes { seance_blocks: 12, claim_maturity_blocks: 0, vesting_blocks: 20, vouch_lock_blocks: 10, claim_ttl_blocks: 30, soul_half_life_blocks: 200 };
    Chain::summon(&home, &SummonOpts { contribution: Some(times), epoch_blocks: 10, ..SummonOpts::new("state", 1) }).unwrap();
    let seed: [u8; 32] = hex::decode(std::fs::read_to_string(home.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
    let mut c = Chain::open(&home).unwrap();
    let users: Vec<Keys> = (1..=USERS as u8).map(|i| Keys::from_seed([i; 32])).collect();
    for u in &users {
        c.faucet(&u.address(), 300 * DROPS_PER_FANG).unwrap();
    }
    c.produce_block().unwrap();
    Sim { home, c, admin: Keys::from_seed(seed), users, agents: (0..AGENTS as u8).map(|i| Keys::from_seed([100 + i; 32])).collect() }
}

/// An unsigned tx (the state machine doesn't check signatures; admission and block verification
/// do), so random sequences don't pay for ML-DSA. Valid for the next 100 blocks.
fn tx(c: &Chain, signer: &Keys, from: &str, action: Action) -> Tx {
    let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), from: from.into(), nonce: c.state.account(from).nonce, fee: 0, tip: 0, valid_until: c.height() + 100, action, memo: String::new(), public_key: hex::encode(signer.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
    tx.fee = min_fee(tx.size());
    tx
}

/// Op kinds: 0–27 below; 28–39 repeat claims, attestations and juror votes, so Séances happen.
const KINDS: u8 = 40;

impl Sim {
    /// Interprets one random op as a tx: (signer, account it acts for, action). `None`: run
    /// empty blocks instead.
    fn op(&self, kind: u8, a: u8, b: u8, amt: u64) -> Option<(Keys, String, Action)> {
        let (a, b) = (a as usize % USERS, b as usize % USERS);
        let user = |i: usize| self.users[i].clone_keys();
        let addr = |i: usize| self.users[i].address();
        let agent = self.agents[b % AGENTS].clone_keys();
        let s = &self.c.state;
        let h = self.c.height();
        let admin = |action: Action| Some((self.admin.clone_keys(), self.admin.address(), action));
        let by = |i: usize, action: Action| Some((user(i), addr(i), action));
        let fang = |n: u64| n * DROPS_PER_FANG;
        let newcomer = Keys::from_seed([200 + b as u8; 32]).address();
        let kind = match kind % KINDS {
            28..=31 => 17,
            32..=35 => 18,
            36..=39 => 25,
            k => k,
        };
        match kind {
            0 => by(a, Action::Transfer { to: addr(b), amount: fang(amt % 20) + 1 }),
            1 => by(a, Action::Transfer { to: newcomer, amount: fang(1) }),
            2 => by(a, Action::RegisterName { name: format!("name{b}") }),
            3 => by(a, Action::AddKey { key_id: key_id(&agent.public_key()), label: "agent".into(), role: KeyRole::Session { limit: fang(5), period_blocks: 10, expires_at: h + 500 } }),
            4 => by(a, Action::RemoveKey { key_id: key_id(&agent.public_key()) }),
            5 => by(a, Action::RegisterFamiliar { key_id: key_id(&agent.public_key()), label: "claude".into() }),
            6 => Some((agent, addr(a), Action::Transfer { to: addr(b), amount: fang(1) })),
            7 => by(a, Action::CreateHunt { repo: "jackdishman/blockula".into(), issue: b as u64 + 1, amount: fang(10 + amt % 20), expires_at: h + 3 + amt % 10, title: "a hunt".into() }),
            8 => by(a, Action::FundHunt { hunt_id: amt % s.next_hunt_id.max(1), amount: fang(1 + amt % 3) }),
            9 => by(a, Action::AwardHunt { hunt_id: amt % s.next_hunt_id.max(1), to: addr(b), pr: amt, familiar_key: None }),
            10 if amt % 2 == 0 => admin(Action::CancelHunt { hunt_id: amt % s.next_hunt_id.max(1) }),
            10 => by(a, Action::CancelHunt { hunt_id: amt % s.next_hunt_id.max(1) }),
            11 => by(a, Action::LinkGithub { handle: format!("user{a}"), gist_id: format!("abc{a}") }),
            12 => by(a, Action::UnlinkGithub { handle: format!("user{a}") }),
            13 => admin(Action::GrantSoul { to: addr(b), amount: (1 + amt % 30) * SOUL, reason: "test".into() }),
            14 => admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: amt % 3 }),
            15 => by(a, Action::Vouch { account: if amt % 3 == 0 { addr(b) } else { newcomer } }),
            16 => by(a, Action::Unvouch { account: if amt % 3 == 0 { addr(b) } else { newcomer } }),
            17 => by(a, Action::SubmitClaim { repo: "jackdishman/blockula".into(), pr: 1 + b as u64 + 10 * (amt % 4), familiar_key: None }),
            18 => {
                // Mostly the oldest pending claim, so attestations land.
                let pending = s.poc.claims.values().find(|c| c.status == crate::poc::ClaimStatus::Pending).map(|c| c.id);
                let claim_id = pending.filter(|_| amt % 5 != 0).unwrap_or(amt % s.poc.next_claim_id.max(1));
                admin(Action::AttestClaim { claim_id, ok: amt % 6 != 0, merge_sha: "ab".repeat(20), note: String::new() })
            }
            19 => by(a, Action::JoinJury { bond: JUROR_BOND }),
            20 => by(a, Action::LeaveJury {}),
            21 => by(a, Action::FundPool { amount: fang(1 + amt % 30) }),
            22 => by(a, Action::WithdrawVested {}),
            23 => admin(Action::Clawback { account: addr(b), reason: "test".into() }),
            24 if amt % 5 == 0 => admin(Action::FlagSybil { account: addr(b), reason: "test".into() }),
            24 => admin(Action::RejectClaim { claim_id: amt % s.poc.next_claim_id.max(1), reason: "test".into() }),
            25 | 26 => {
                // A seated juror seals, then reveals, all-'a' votes in the open Séance.
                let id = s.poc.open_seance?;
                let seance = &s.poc.seances[&id];
                let seat = seance.jurors.get(&addr(a))?;
                let votes: String = seat.pairs.iter().map(|_| 'a').collect();
                if seat.commitment.is_none() && h < seance.commit_until {
                    by(a, Action::CommitVotes { seance_id: id, commitment: seance_commitment(id, &addr(a), &votes, &[], "salt") })
                } else {
                    by(a, Action::RevealVotes { seance_id: id, votes, spam: vec![], salt: "salt".into() })
                }
            }
            _ => None,
        }
    }

    fn block(&mut self, txs: Vec<Tx>) {
        self.c.produce_block_with(txs).unwrap();
        // `execute` already runs `debug_check`; this is the property, stated where it's tested.
        assert_eq!(self.c.state.root_hash(), self.c.state.full_root(), "incremental root == full rebuild");
        self.c.state.check_invariants().unwrap();
    }

    /// Runs the ops, a few txs per block (one per sender, so nonces line up).
    fn run(&mut self, ops: &[(u8, u8, u8, u64)]) {
        let mut txs: Vec<Tx> = vec![];
        for &(kind, a, b, amt) in ops {
            match self.op(kind, a, b, amt) {
                Some((_, from, _)) => {
                    if txs.len() >= 4 || txs.iter().any(|t| t.from == from) {
                        self.block(std::mem::take(&mut txs));
                    }
                    // Built against the state after the last block (ops may depend on it).
                    if let Some((signer, from, action)) = self.op(kind, a, b, amt) {
                        txs.push(tx(&self.c, &signer, &from, action));
                    }
                }
                None => {
                    self.block(std::mem::take(&mut txs));
                    for _ in 0..amt % 6 {
                        self.block(vec![]);
                    }
                }
            }
        }
        self.block(txs);
    }

    /// Runs empty blocks until a Séance opens (they open on epoch boundaries, on a cadence).
    fn wait_for_seance(&mut self) -> Option<u64> {
        for _ in 0..100 {
            if self.c.state.poc.open_seance.is_some() {
                break;
            }
            self.block(vec![]);
        }
        self.c.state.poc.open_seance
    }

    /// If a Séance is open, every seated juror seals and then reveals, and it runs to its close.
    fn drive_seance(&mut self) {
        let Some(sid) = self.c.state.poc.open_seance else { return };
        let everyone: Vec<(u8, u8, u8, u64)> = (0..USERS as u8).map(|i| (25, i, 0, 0)).collect();
        if self.c.height() < self.c.state.poc.seances[&sid].commit_until {
            self.run(&everyone);
        }
        while self.c.height() < self.c.state.poc.seances[&sid].commit_until {
            self.block(vec![]);
        }
        self.run(&everyone);
        while self.c.state.poc.open_seance == Some(sid) {
            self.block(vec![]);
        }
    }

    /// A restart reaches the same state (the database is checked against the root and the log),
    /// and the snapshot encoding round-trips it.
    fn check_restart_and_snapshot(self) {
        let (root, height) = (self.c.state.root(), self.c.height());
        let blocks: Vec<crate::types::Block> = (1..=height).filter_map(|h| self.c.block(h)).collect();
        let (txs, failed) = (blocks.iter().map(|b| b.txs.len()).sum::<usize>(), blocks.iter().map(|b| b.failed.len()).sum::<usize>());
        let mut per_ns = std::collections::BTreeMap::new();
        self.c.state.for_each_record(|n, _, _| *per_ns.entry(n).or_insert(0) += 1);
        println!("{height} blocks, {txs} txs ({failed} failed), Séances {}, records by namespace {per_ns:?}", self.c.state.poc.next_seance_id);
        let roots: Vec<String> = (1..=height).map(|h| self.c.root_at(h).unwrap()).collect();
        let records = all_records(&self.c.state);
        drop(self.c);
        let c = Chain::open(&self.home).unwrap();
        assert_eq!((c.state.root(), c.height()), (root.clone(), height));
        assert_eq!((1..=height).map(|h| c.root_at(h).unwrap()).collect::<Vec<_>>(), roots);
        let mut bytes = vec![];
        crate::snapshot::write_state(&c.state, &mut bytes).unwrap();
        let back = crate::snapshot::read_state(&mut bytes.as_slice()).unwrap();
        assert_eq!(back.root(), root);
        assert_eq!(all_records(&back), records);
        std::fs::remove_dir_all(&self.home).ok();
    }
}

fn all_records(s: &State) -> Vec<(u8, Vec<u8>, Vec<u8>)> {
    let mut v = vec![];
    s.for_each_record(|n, k, val| v.push((n, k.to_vec(), val.to_vec())));
    v
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, max_shrink_iters: 32, failure_persistence: None, ..ProptestConfig::default() })]

    /// Random sequences of real txs (accounts, keys, names, hunts, GitHub, Familiars, Souls,
    /// vouches, claims, juries, Séances, grants, sybil flags) and empty blocks spanning epoch
    /// boundaries: after every block the incremental root equals a full rebuild and the
    /// invariants hold; at the end, replay and the snapshot encoding reproduce the state.
    #[test]
    fn random_txs_keep_the_root_and_invariants(ops in prop::collection::vec((0..KINDS, any::<u8>(), any::<u8>(), any::<u64>()), 20..120)) {
        let mut s = sim("prop");
        // Bootstrap enough Soul and a pool repo that claims and juries happen at all (the last
        // user stays a newcomer, who needs a vouch).
        let mut boot: Vec<(u8, u8, u8, u64)> = vec![(14, 0, 0, 1), (21, 0, 0, 29)];
        boot.extend((0..USERS as u8 - 1).map(|i| (13, 0, i, 20)));
        boot.extend((0..USERS as u8).flat_map(|i| [(11, i, 0, 0), (19, i, 0, 0)]));
        s.run(&boot);
        // Halfway, any open Séance is judged, so the rest runs with grants, scored claims and
        // vesting around.
        let (first, second) = ops.split_at(ops.len() / 2);
        s.run(first);
        s.drive_seance();
        s.run(second);
        s.drive_seance();
        s.check_restart_and_snapshot();
    }
}

/// A scripted run through every subsystem, including a full Séance (the random test reaches one
/// only sometimes).
#[test]
fn scripted_run_through_a_seance() {
    let mut s = sim("script");
    let mut ops: Vec<(u8, u8, u8, u64)> = vec![(14, 0, 0, 1)];
    for i in 0..USERS as u8 {
        ops.extend([(13, 0, i, 20), (11, i, 0, 0), (19, i, 0, 0)]);
    }
    ops.extend([(21, 0, 0, 29), (17, 0, 0, 0), (17, 1, 1, 0), (17, 2, 2, 0)]);
    s.run(&ops);
    s.run(&[(18, 0, 0, 1), (18, 0, 0, 1), (18, 0, 0, 1)]);
    let failed: Vec<String> = (1..=s.c.height()).flat_map(|h| s.c.block(h).unwrap().failed.into_values()).collect();
    assert!(failed.is_empty(), "{failed:?}");
    let sid = s.wait_for_seance().expect("3 verified claims and 6 jurors open a Séance at the next opening");
    s.drive_seance();
    let r = s.c.state.poc.seances[&sid].results.clone().unwrap();
    assert!(r.accepted > 0 && r.no_shows == 0, "the Séance judged the claims: {r:?}");
    // Rewards vest; everyone withdraws what has.
    for _ in 0..20 {
        s.block(vec![]);
    }
    s.run(&(0..USERS as u8).map(|i| (22, i, 0, 0)).collect::<Vec<_>>());
    s.check_restart_and_snapshot();
}

/// A transfer block changes exactly the records it should; mempool dry runs and proposals
/// (apply, then roll back) leave nothing for the next commit.
#[test]
fn commits_write_only_touched_records() {
    let mut s = sim("touched");
    let (a, b) = (s.users[0].clone_keys(), s.users[1].address());
    let t = tx(&s.c, &a, &a.address(), Action::Transfer { to: b.clone(), amount: DROPS_PER_FANG });
    s.c.submit(t.clone()).unwrap();
    s.c.submit(tx(&s.c, &s.users[2].clone_keys(), &s.users[2].address(), Action::RegisterName { name: "dryrun".into() })).unwrap();
    assert!(s.c.state.accounts.dirty().is_empty() && s.c.state.names.dirty().is_empty(), "submit's dry run left no dirty keys");
    // What execute does, by hand, to see the commit.
    let me = s.c.me().unwrap();
    s.c.state.apply(&t, "state", &me, s.c.height() + 1).unwrap();
    let commit = s.c.state.commit_block().unwrap();
    let mut accounts = [a.address(), b, me];
    accounts.sort();
    let touched: Vec<(u8, Vec<u8>)> = commit.records.iter().map(|r| (r.namespace, r.key.clone())).collect();
    let mut expected: Vec<(u8, Vec<u8>)> = vec![(ns::META, meta::BURNED.to_vec()), (ns::META, meta::SUPPLY.to_vec())];
    expected.extend(accounts.iter().map(|a| (ns::ACCOUNTS, a.as_bytes().to_vec())));
    assert_eq!(touched, expected);
    assert_eq!(commit.root, s.c.state.full_root());
    assert!(s.c.state.commit_block().unwrap().records.is_empty(), "nothing left to commit");
    std::fs::remove_dir_all(&s.home).ok();
}

#[test]
fn state_proofs_verify_against_the_root() {
    let mut s = sim("proofs");
    s.run(&[(2, 0, 3, 0), (7, 1, 0, 0)]);
    let st = &s.c.state;
    let root = st.root_hash();
    let alice = s.users[0].address();
    for (n, key) in [(ns::ACCOUNTS, alice.as_bytes().to_vec()), (ns::NAMES, b"name3".to_vec()), (ns::HUNTS, 0u64.to_be_bytes().to_vec()), (ns::META, meta::SUPPLY.to_vec())] {
        let value = st.record(n, &key).expect("record exists");
        let p = st.prove(n, &key).unwrap();
        assert!(tree::verify(&root, n, &key, Some(&value), &p), "inclusion");
        let mut tampered = value.clone();
        tampered[0] ^= 1;
        assert!(!tree::verify(&root, n, &key, Some(&tampered), &p), "tampered value");
        assert!(!tree::verify(&root, n, &key, None, &p), "exclusion of a present key");
        assert!(!tree::verify(&root, n ^ 0x40, &key, Some(&value), &p), "wrong namespace");
        assert!(!tree::verify(&root, n, b"someone else", Some(&value), &p), "wrong key");
    }
    // The decoded value is the record: a balance proven from the root.
    let acct: Account = codec::decode(&st.record(ns::ACCOUNTS, alice.as_bytes()).unwrap()).unwrap();
    assert_eq!(acct.balance, st.account(&alice).balance);
    // Absent records are proven absent, and that proof can't vouch for a value.
    let nobody = Keys::from_seed([99; 32]).address();
    let p = st.prove(ns::ACCOUNTS, nobody.as_bytes()).unwrap();
    assert!(st.record(ns::ACCOUNTS, nobody.as_bytes()).is_none());
    assert!(tree::verify(&root, ns::ACCOUNTS, nobody.as_bytes(), None, &p));
    assert!(!tree::verify(&root, ns::ACCOUNTS, nobody.as_bytes(), Some(&codec::encode(&Account::default())), &p));
    assert!(!tree::verify(&root, ns::ACCOUNTS, alice.as_bytes(), None, &p), "another key's exclusion proof");
    // A proof is for one root: after the next block, old proofs of changed records fail.
    let old = st.prove(ns::ACCOUNTS, alice.as_bytes()).unwrap();
    let value = st.record(ns::ACCOUNTS, alice.as_bytes()).unwrap();
    s.run(&[(0, 0, 1, 1)]);
    assert!(!tree::verify(&s.c.state.root_hash(), ns::ACCOUNTS, alice.as_bytes(), Some(&value), &old));
    std::fs::remove_dir_all(&s.home).ok();
}

#[test]
fn snapshots_reject_tampering() {
    let mut s = sim("snap");
    s.run(&[(2, 0, 3, 0), (7, 1, 0, 0), (13, 0, 2, 5)]);
    let mut bytes = vec![];
    crate::snapshot::write_state(&s.c.state, &mut bytes).unwrap();
    let root = s.c.state.root();
    let mut back = crate::snapshot::read_state(&mut bytes.as_slice()).unwrap();
    assert_eq!((back.root(), back.full_root()), (root.clone(), s.c.state.root_hash()));
    assert!(back.commit_block().unwrap().records.is_empty(), "a restored state has nothing to commit");
    // A flipped byte in a value: a different state (or no state at all), never the same root.
    let at = bytes.len() - 5;
    let mut bad = bytes.clone();
    bad[at] ^= 0x10;
    assert!(crate::snapshot::read_state(&mut bad.as_slice()).map(|st| st.root()).ok() != Some(root.clone()));
    // Trailing bytes, truncation, a wrong schema or magic.
    assert!(crate::snapshot::read_state(&mut [bytes.as_slice(), &[0]].concat().as_slice()).is_err());
    assert!(crate::snapshot::read_state(&mut &bytes[..bytes.len() - 1]).is_err());
    let mut schema = bytes.clone();
    schema[8] = 9;
    assert!(crate::snapshot::read_state(&mut schema.as_slice()).err().unwrap().to_string().contains("schema"));
    assert!(crate::snapshot::read_state(&mut &b"NOTSTATE"[..]).is_err());
    // Records must come in canonical order, without duplicates or unknown namespaces.
    let records = all_records(&s.c.state);
    let mut swapped = records.clone();
    let n = swapped.len();
    swapped.swap(n - 1, n - 2);
    assert!(State::from_records(swapped.into_iter().map(Ok)).err().unwrap().to_string().contains("order"));
    let mut dup = records.clone();
    dup.push(records.last().unwrap().clone());
    assert!(State::from_records(dup.into_iter().map(Ok)).is_err());
    let mut unknown = records.clone();
    unknown.push((200, b"x".to_vec(), vec![]));
    assert!(State::from_records(unknown.into_iter().map(Ok)).err().unwrap().to_string().contains("namespace"));
    assert!(State::from_records(records[1..].iter().cloned().map(Ok)).err().unwrap().to_string().contains("scalar"));
    std::fs::remove_dir_all(&s.home).ok();
}

/// Namespaces are never shared, and each protocol v1 module's sit in its reserved range (the
/// maps a state lists are checked against them too); META keys are distinct.
#[test]
fn namespaces_and_meta_keys_are_unique() {
    let reserved = [
        (ns::STAKE_POOLS, 32..=36),
        (ns::STAKE_DELEGATIONS, 32..=36),
        (ns::STAKE_UNBONDING, 32..=36),
        (ns::STAKE_EVIDENCE, 32..=36),
        (ns::STAKE_LIVENESS, 32..=36),
        (ns::GOV_PROPOSALS, 37..=42),
        (ns::GOV_VOTES, 37..=42),
        (ns::GOV_MULTISIGS, 37..=42),
        (ns::GOV_MULTISIG_PROPOSALS, 37..=42),
        (ns::GOV_DISTRIBUTIONS, 37..=42),
        (ns::GOV_DISTRIBUTION_CLAIMS, 37..=42),
        (ns::RESERVED_NAMES, 43..=43),
        (ns::BEACON_COMMITS, 44..=46),
        (ns::BEACON_REVERTS, 44..=46),
        (ns::BEACON_VALUES, 44..=46),
        (ns::FEATURES, 47..=47),
    ];
    let mut seen = std::collections::BTreeSet::from([ns::META]);
    let st = State::default();
    for (n, _) in st.tables() {
        assert!(seen.insert(n) || reserved.iter().any(|(r, _)| *r == n), "namespace {n} is used twice");
    }
    for (n, range) in reserved {
        assert!(range.contains(&n), "namespace {n} is outside its module's range");
        assert!(!(1..=25).contains(&n) && !(48..=63).contains(&n), "namespace {n} collides with the base state or Ghost Mode");
        seen.insert(n);
    }
    assert_eq!(seen.len(), 1 + 18 + 16, "every namespace is distinct");
    let keys = [meta::BURNED, meta::CHAIN_PARAMS, meta::NEXT_HUNT_ID, meta::PARAMS, meta::POC, meta::SCHEMA, meta::SUPPLY, meta::STAKING, meta::GOVERNANCE, meta::ECON, meta::BEACON, meta::UPGRADE];
    assert_eq!(keys.iter().collect::<std::collections::BTreeSet<_>>().len(), keys.len());
    assert!(st.meta_records().iter().all(|(k, _)| keys.contains(k)), "every scalar record uses a declared key");
}

/// Format change detectors: a record's bytes and a fixed state's root. The root was also
/// computed by an independent JS implementation of the record layout and tree (noble BLAKE3),
/// recomputed when `Params` gained `address_prefix` (P-WIRE), for the protocol v1 scaffold
/// (schema 2: `chain_params`, the upgrade schedule, account locks) and for the beacon (schema 3:
/// the PoC scalars without the block-hash seed).
#[test]
fn golden_encodings() {
    let acct = Account { balance: 5, nonce: 2, keys: Default::default(), locks: vec![] };
    assert_eq!(hex::encode(codec::encode(&acct)), "050000000000000002000000000000000000000000000000");
    let mut st = State::default();
    st.accounts.insert("boo1alice".into(), acct);
    st.supply = 5;
    st.commit_block().unwrap();
    assert_eq!(st.root_hash(), st.full_root());
    assert_eq!(st.record_count(), 9);
    assert_eq!(st.root(), "7d6ceaa88b90815fb334c8117b391a05e9276b495d33513f327116d8516a2bbf");
}

fn million_accounts() -> State {
    let mut st = State::default();
    for i in 0..1_000_000u64 {
        st.accounts.insert(crypto::address_from_bytes(&crypto::hash(&i.to_le_bytes())), Account { balance: DROPS_PER_FANG, nonce: i % 7, ..Default::default() });
    }
    st.supply = 1_000_000 * DROPS_PER_FANG;
    st
}

/// Cloning shares structure: a million-account state (and its tree) clones in microseconds, and
/// the clones evolve independently.
#[test]
fn cloning_a_million_accounts_is_constant_time() {
    let mut st = million_accounts();
    st.commit_block().unwrap();
    let t = Instant::now();
    let snapshot = st.clone();
    let took = t.elapsed();
    println!("cloned a 1M-account state in {took:?}");
    assert!(took.as_micros() < 1_000, "clone took {took:?}");
    let alice = st.accounts.keys().next().unwrap().clone();
    st.accounts.get_mut(&alice).unwrap().balance += 1;
    st.supply += 1;
    st.commit_block().unwrap();
    assert_eq!(snapshot.account(&alice).balance, DROPS_PER_FANG);
    assert_ne!(snapshot.root(), st.root());
    assert!(tree::verify(&snapshot.root_hash(), ns::ACCOUNTS, alice.as_bytes(), Some(&codec::encode(&snapshot.account(&alice))), &snapshot.prove(ns::ACCOUNTS, alice.as_bytes()).unwrap()));
}

/// `cargo test --release state::tests::bench -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_million_accounts() {
    let t = Instant::now();
    let mut st = million_accounts();
    println!("built 1M accounts in memory: {:?}", t.elapsed());
    let t = Instant::now();
    st.commit_block().unwrap();
    println!("first commit (builds the tree from all {} records): {:?}", st.record_count(), t.elapsed());
    let t = Instant::now();
    let full = st.full_root();
    println!("full root from scratch (reference): {:?}", t.elapsed());
    assert_eq!(full, st.root_hash());
    let t = Instant::now();
    let _clone = st.clone();
    println!("clone: {:?}", t.elapsed());
    let keys: Vec<String> = st.accounts.keys().step_by(997).cloned().collect();
    let mut times = vec![];
    for block in 0..20 {
        // ~400 touched records: 200 transfers between random existing accounts.
        for i in 0..200 {
            let (from, to) = (&keys[(block * 400 + 2 * i) % keys.len()], &keys[(block * 400 + 2 * i + 1) % keys.len()]);
            st.accounts.get_mut(from).unwrap().balance -= 1;
            st.accounts.get_mut(to).unwrap().balance += 1;
            st.accounts.get_mut(from).unwrap().nonce += 1;
        }
        let t = Instant::now();
        let commit = st.commit_block().unwrap();
        times.push(t.elapsed());
        assert_eq!(commit.records.len(), 400);
    }
    println!("commits of 400 touched records over 1M, per block: {times:?}");
    times.sort();
    println!("commit of 400 touched records over 1M: median {:?}, max {:?}", times[times.len() / 2], times[times.len() - 1]);
    // The same with a clone of the previous state alive across each commit (an API snapshot),
    // so every write copies the paths it shares.
    let mut times = vec![];
    for block in 20..40 {
        let _snapshot = st.clone();
        for i in 0..200 {
            let (from, to) = (&keys[(block * 400 + 2 * i) % keys.len()], &keys[(block * 400 + 2 * i + 1) % keys.len()]);
            st.accounts.get_mut(from).unwrap().balance -= 1;
            st.accounts.get_mut(to).unwrap().balance += 1;
        }
        let t = Instant::now();
        st.commit_block().unwrap();
        times.push(t.elapsed());
    }
    times.sort();
    println!("same, with a snapshot clone held: median {:?}, max {:?}", times[times.len() / 2], times[times.len() - 1]);
    let key = keys[0].clone();
    let t = Instant::now();
    for _ in 0..1000 {
        st.prove(ns::ACCOUNTS, key.as_bytes()).unwrap();
    }
    println!("proof: {:?} each, {} siblings", t.elapsed() / 1000, st.prove(ns::ACCOUNTS, key.as_bytes()).unwrap().siblings.len());
    let t = Instant::now();
    let mut bytes = vec![];
    crate::snapshot::write_state(&st, &mut bytes).unwrap();
    println!("snapshot encode: {:?} ({} MB raw)", t.elapsed(), bytes.len() >> 20);
    let t = Instant::now();
    let back = crate::snapshot::read_state(&mut bytes.as_slice()).unwrap();
    println!("snapshot decode + tree rebuild: {:?}", t.elapsed());
    assert_eq!(back.root_hash(), st.root_hash());
}
