use super::*;
use crate::chain::{now_ms, ApplyMode, Chain, Genesis, SummonOpts};
use crate::crypto::{self, Keys};
use crate::types::tests::{ghost_vector_actions, program_vector_actions, small_set_vector_actions, v1_vector_actions, vector_actions, vector_address, vector_tx};
use crate::types::{min_fee, Binding, HexBytes, InitCall, ProgramCall, ProgramPolicy, ProposalKind, DROPS_PER_FANG};
use crate::upgrade::{Feature, ScheduledUpgrade, PROGRAMS_FEATURE, PROGRAMS_VERSION};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn at(version: u32, activation_height: u64) -> ScheduledUpgrade {
    ScheduledUpgrade { version, activation_height }
}

/// A chain with no upgrade schedule (testnet-2) runs v1 at every height, so programs are dormant
/// there forever.
#[test]
fn programs_are_dormant_on_a_v1_chain() {
    let st = State::default();
    for h in [0, 1, 1_000_000, u64::MAX] {
        assert_eq!(status(&st, h), Status::Dormant, "height {h}");
    }
}

/// From v2 on, programs exist only once they've been switched on, which creates META `programs`.
/// Without it they're dormant, on a chain born at v2 and on one upgraded to it later, and the
/// feature flag on its own (a state `check_invariants` refuses) doesn't change that.
#[test]
fn programs_are_dormant_at_v2_until_switched_on() {
    let mut st = State::default();
    st.upgrade.schedule = vec![at(PROGRAMS_VERSION, 1)];
    for h in [0, 1, 2, u64::MAX] {
        assert_eq!(status(&st, h), Status::Dormant, "born at v2, height {h}");
    }
    st.upgrade.schedule = vec![at(PROGRAMS_VERSION, 100)];
    for h in [99, 100, 101] {
        assert_eq!(status(&st, h), Status::Dormant, "upgraded to v2 at 100, height {h}");
    }
    st.upgrade.features.insert(PROGRAMS_FEATURE.to_string(), Feature { activated_at: 100 });
    for h in [99, 100, 101, u64::MAX] {
        assert_eq!(status(&st, h), Status::Dormant, "the flag without META, height {h}");
    }
}

/// Once META `programs` exists, `status` reads the feature and the pause (§1.1): `On` with the
/// feature, `ExitOnly` without it, and `Paused { until }` over a running pause's `[from, until)`
/// whatever the feature says, the last run's record kept in META after it lapses changing
/// nothing. Below v2 it's `Dormant` first, whatever META holds.
#[test]
fn status_reads_meta_the_feature_and_the_pause() {
    let mut st = State::default();
    st.upgrade.schedule = vec![at(PROGRAMS_VERSION, 10)];
    on_activate(&mut st, None);
    for h in [9, 10, 1_000] {
        assert_eq!(status(&st, h), if h < 10 { Status::Dormant } else { Status::ExitOnly }, "META without the feature, height {h}");
    }
    st.upgrade.features.insert(PROGRAMS_FEATURE.to_string(), Feature { activated_at: 10 });
    assert_eq!((status(&st, 9), status(&st, 10), status(&st, u64::MAX)), (Status::Dormant, Status::On, Status::On));
    st.programs.meta.as_mut().unwrap().pause = Some(state::Pause { from: 20, until: 30, started: 20, signers: vec![vector_address(41)] });
    for (h, on) in [(19, Status::On), (20, Status::Paused { until: 30 }), (29, Status::Paused { until: 30 }), (30, Status::On), (u64::MAX, Status::On)] {
        assert_eq!(status(&st, h), on, "feature on, height {h}");
    }
    st.upgrade.features.remove(PROGRAMS_FEATURE);
    for (h, off) in [(19, Status::ExitOnly), (25, Status::Paused { until: 30 }), (30, Status::ExitOnly)] {
        assert_eq!(status(&st, h), off, "feature off, height {h}");
    }
    st.upgrade.schedule = vec![at(PROGRAMS_VERSION, 100)];
    assert_eq!(status(&st, 25), Status::Dormant, "below v2 nothing else is read");
}

/// Program transactions are exactly those whose action has tag 62, 63 or 64: never one of v1's
/// or Ghost Mode's, nor `ProgramsPause` (69). Pinned by the wire tag, so the vectors of every tag
/// the codec decodes belong in this list.
#[test]
fn program_transactions_are_tags_62_to_64() {
    let actions: Vec<_> = vector_actions().into_iter().chain(v1_vector_actions()).chain(ghost_vector_actions()).chain(small_set_vector_actions()).chain(program_vector_actions()).collect();
    let tags: BTreeSet<u8> = actions.iter().map(|a| a.to_bytes()[0]).collect();
    assert_eq!(tags, (0..=54).chain(60..=64).chain([69]).collect(), "every tag the codec decodes");
    for action in actions {
        let tag = action.to_bytes()[0];
        assert_eq!(is_program_tx(&vector_tx(action.clone())), (62..=64).contains(&tag), "tag {tag}");
        assert_eq!(action.is_program(), (62..=64).contains(&tag) || tag == 69, "tag {tag}");
    }
}

/// Protocol v2's constants (§5.3; frozen with its other program rules, §9.2). `MAX_FUEL_RATE`
/// follows from W1's `MAX_RESERVATION`, so a change there that would move the fuel rate's
/// ceiling fails here.
#[test]
fn v2_constants_keep_their_values() {
    assert_eq!(RECORD_BYTES, 256);
    assert_eq!(MAX_RESERVATION, 1 << 26);
    assert_eq!(MAX_FUEL_RATE, (1 << 38) - 1);
    assert!(MAX_FUEL_RATE.checked_mul(MAX_RESERVATION).is_some(), "the largest fuel charge fits a u64");
    // The tx codec's bounds (§2, §5.3): its own, and W1's that it enforces.
    use blockula_vm::config::{MAX_BINDINGS, MAX_CALLS, MAX_CODE_BYTES, MAX_INPUT, MAX_INTERFACE_BYTES};
    assert_eq!(MAX_CODE_TX_BYTES, 320 * 1024);
    assert_eq!((MAX_CODE_BYTES, MAX_INTERFACE_BYTES, MAX_INPUT, MAX_CALLS, MAX_BINDINGS), (256 * 1024, 16 * 1024, 32 * 1024, 8, 16));
    assert_eq!(blockula_vm::meter::MAX_TX_FUEL, 20_000_000);
    assert_eq!((crate::types::WASM_HEADER, crate::types::MAX_METHOD), ([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00], 32));
}

/// A program's transfer that opens an account credits at least what a native one must
/// (docs/PROGRAMS-V0.md A30): the VM keeps its own copy of the node's minimum, which must not drift.
#[test]
fn programs_open_accounts_with_the_native_minimum() {
    assert_eq!(blockula_vm::config::MIN_NEW_ACCOUNT, crate::types::MIN_NEW_ACCOUNT);
}

/// The VM's and the engine's crates panic on arithmetic overflow in the node's release build, as
/// in their own workspace's (node/Cargo.toml says why). An engine crate that a version bump adds
/// to the lock without a profile entry fails here.
#[test]
fn release_profile_covers_the_vm() {
    let manifest = include_str!("../../Cargo.toml");
    let lock = include_str!("../../Cargo.lock");
    let crates: BTreeSet<&str> = lock
        .lines()
        .filter_map(|l| l.strip_prefix("name = \""))
        .map(|l| l.trim_end_matches('"'))
        .filter(|n| *n == "blockula-vm" || n.starts_with("soroban-") || n.starts_with("wasmi") || n.starts_with("wasmparser"))
        .collect();
    assert!(crates.is_superset(&BTreeSet::from(["blockula-vm", "soroban-wasmi", "wasmi_core", "wasmparser"])), "{crates:?}");
    for c in crates {
        assert!(manifest.contains(&format!("[profile.release.package.{c}]\noverflow-checks = true")), "node/Cargo.toml needs [profile.release.package.{c}] overflow-checks = true");
    }
    assert!(!manifest.contains("panic = \"abort\""), "the VM catches panics: it needs panic = \"unwind\"");
}

/// Execution order is the block's and diffs apply sorted (§6.1): nothing in program code may
/// iterate a collection whose order depends on the process. W1's clippy.toml bans hash
/// collections in node/vm; the node's crate has no per-module Clippy configuration, so this reads
/// every file under node/src/programs (this one included) and fails on one that names them.
#[test]
fn no_hash_collections() {
    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/programs");
    let mut files = vec![];
    rust_files(&root, &mut files);
    let names: BTreeSet<String> = files.iter().map(|f| f.strip_prefix(&root).unwrap().to_string_lossy().into_owned()).collect();
    assert!(names.is_superset(&["mod.rs", "admit.rs", "digest.rs", "describe.rs", "tests.rs"].map(String::from).into()), "{names:?}");
    // Spelled in pieces, so that this file doesn't name them itself.
    let banned = [["Hash", "Map"].concat(), ["Hash", "Set"].concat(), ["hash", "_map"].concat(), ["hash", "_set"].concat()];
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for b in &banned {
            assert!(!text.contains(b.as_str()), "{} names {b}: program code iterates only ordered collections (BTreeMap, BTreeSet, sorted Vecs)", f.display());
        }
    }
}

fn at_height(version: u32, activation_height: u64, features: &[&str]) -> State {
    let mut st = State::default();
    if version > 1 {
        st.upgrade.schedule = vec![at(version, activation_height)];
    }
    for f in features {
        st.upgrade.features.insert(f.to_string(), Feature { activated_at: activation_height });
    }
    st
}

/// While programs are dormant, admission refuses every program action, whatever the state holds
/// besides (the `programs` flag without program state included), and lets every other action by:
/// below v2 (testnet-2 forever), and at v2 before programs switch on (blockula-1 at launch).
#[test]
fn every_program_action_is_refused_while_dormant() {
    let states = [("a v1 chain", at_height(1, 0, &[])), ("born at v2", at_height(PROGRAMS_VERSION, 1, &[])), ("upgraded to v2 at 100", at_height(PROGRAMS_VERSION, 100, &[])), ("v2 with the flag alone", at_height(PROGRAMS_VERSION, 1, &[PROGRAMS_FEATURE]))];
    for (label, st) in &states {
        for h in [1, 2, 99, 100, 101, 1_000_000] {
            assert_eq!(status(st, h), Status::Dormant, "{label}, height {h}");
            for a in program_vector_actions() {
                let e = admit(st, &vector_tx(a.clone()), h, ApplyMode::Block).unwrap_err().to_string();
                assert_eq!(e, format!("programs are off: a {} transaction is refused until protocol v2 is in force and governance switches the `programs` feature on", a.kind()), "{label}, height {h}");
            }
            for a in vector_actions().into_iter().chain(v1_vector_actions()).chain(ghost_vector_actions()).chain(small_set_vector_actions()) {
                admit(st, &vector_tx(a.clone()), h, ApplyMode::Block).unwrap_or_else(|e| panic!("{label}, height {h}, {}: {e}", a.kind()));
            }
        }
    }
    // No program exists while they're dormant, so no sender is one.
    assert!(!is_program(&states[1].1, &vector_address(41)));
}

/// A program has no keys, so once programs exist admission refuses any transaction from a
/// program's account, of any kind (§2.1): `is_program` reads namespace 66. The check is gated on
/// `status`, so a state that held a record while dormant (one `check_invariants` refuses) wouldn't
/// wake it.
#[test]
fn nothing_acts_from_a_program() {
    let program = vector_address(41);
    let record = state::ProgramRecord {
        creator: vector_address(42),
        created_at: 1,
        wasm_hash: [1; 32],
        interface_hash: [2; 32],
        version: 1,
        authority: None,
        upgrade_delay_blocks: 0,
        policy: 0,
        accepts_transfers: true,
        items: 0,
        bytes: 0,
        deposit: 0,
        sponsored: 0,
        record_deposit: 0,
        record_payer: vector_address(42),
    };
    let mut st = at_height(PROGRAMS_VERSION, 1, &[PROGRAMS_FEATURE]);
    on_activate(&mut st, None);
    st.programs.records.insert(program.clone(), state::Versioned(record));
    assert!(is_program(&st, &program) && !is_program(&st, &vector_address(42)));
    assert_eq!(status(&st, 5), Status::On);
    for a in vector_actions().into_iter().chain(v1_vector_actions()).chain(small_set_vector_actions()) {
        let e = admit(&st, &Tx { from: program.clone(), ..vector_tx(a.clone()) }, 5, ApplyMode::Block).unwrap_err().to_string();
        assert_eq!(e, format!("{} is a program: programs have no keys, so no transaction acts from one", crate::chain::short(&program)), "{}", a.kind());
        admit(&st, &Tx { from: vector_address(42), ..vector_tx(a.clone()) }, 5, ApplyMode::Block).unwrap_or_else(|e| panic!("{}: {e}", a.kind()));
    }
    // Below v2 it's `Dormant`, and the record isn't read.
    st.upgrade.schedule = vec![at(PROGRAMS_VERSION, 100)];
    admit(&st, &Tx { from: program, ..vector_tx(Action::Transfer { to: vector_address(42), amount: 1 }) }, 5, ApplyMode::Block).unwrap();
}

/// A dev chain's home (one validator, `boo` addresses), born at protocol `version`, and the chain
/// opened there as its validator.
fn dev_chain(tag: &str, version: u32) -> (PathBuf, Chain) {
    dev_chain_with(tag, version, &[])
}

/// [`dev_chain`], its genesis listing `features` too (`programs`, on a chain born at v2, creates
/// the program state at block 0), with a 10 FANG proposal deposit.
fn dev_chain_with(tag: &str, version: u32, features: &[&str]) -> (PathBuf, Chain) {
    let home = std::env::temp_dir().join(format!("blockula-programs-{tag}-{}-{}", now_ms(), std::process::id()));
    let mut o = SummonOpts::new("programs-dev-1", 1);
    o.address_prefix = crypto::ADDRESS_HRP.into();
    o.params.insert("proposal_deposit".into(), 10 * DROPS_PER_FANG);
    Chain::summon(&home, &o).unwrap();
    if version > 1 || !features.is_empty() {
        let path = home.join("genesis.json");
        let mut g: Genesis = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        if version > 1 {
            g.upgrades = vec![at(version, 1)];
        }
        g.features = features.iter().map(|f| f.to_string()).collect();
        g.check().unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&g).unwrap()).unwrap();
    }
    let c = Chain::open(&home).unwrap();
    (home, c)
}

/// `action` from `k`'s account at its next nonce, paying the base fee at its floor.
fn signed(c: &Chain, k: &Keys, action: Action) -> Tx {
    let from = k.address_with(c.state.params.prefix());
    let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), nonce: c.next_nonce(&from), from, fee: 0, tip: 0, valid_until: c.height() + 100, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
    tx.fee = min_fee(tx.size());
    tx.sign(k)
}

/// The acceptance of WBS S1 on a running chain, below v2 (testnet-2's case) and on one born at v2
/// without programs (blockula-1's at launch): each program action, signed by an owner key on an
/// ordinary account (and the pause by the validator from its own account too), is refused on
/// every path. Mempool admission refuses it (`Err`); a block that carries it anyway skips it as
/// unadmittable, as verification and replay do (the same admission). Its sender is charged
/// nothing and its nonce stays (the proposer pays for the junk, as for any skipped tx).
#[test]
fn program_actions_are_refused_on_every_path_while_dormant() {
    for version in [1, PROGRAMS_VERSION] {
        let (home, mut c) = dev_chain(&format!("dormant-v{version}"), version);
        let validator = c.keys.as_ref().unwrap().clone_keys();
        let alice = Keys::from_seed([61; 32]);
        c.faucet(&alice.address(), 100 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        assert_eq!((c.height(), upgrade::version_at(&c.state, c.height() + 1)), (1, version));
        assert_eq!(status(&c.state, c.height() + 1), Status::Dormant);
        // Each at the sender's next nonce: they're alternatives, and each is refused on its own.
        let mut txs: Vec<Tx> = program_vector_actions().into_iter().map(|a| signed(&c, &alice, a)).collect();
        txs.push(signed(&c, &validator, Action::ProgramsPause {}));
        let refused = |e: &str, tx: &Tx| assert!(e.contains(&format!("programs are off: a {} transaction is refused", tx.action.kind())), "v{version}, {}: {e}", tx.action.kind());
        let accounts = |c: &Chain| [&alice, &validator].map(|k| c.state.account(&k.address()).clone());
        let before = accounts(&c);
        // The mempool.
        for tx in &txs {
            refused(&format!("{:#}", c.submit(tx.clone()).unwrap_err()), tx);
        }
        assert!(c.mempool.is_empty());
        // A block that carries them: skipped, each with its reason, none charged.
        c.produce_block_with(txs.clone()).unwrap();
        let b = c.last_block().unwrap();
        assert!(b.txs.is_empty() && b.failed.is_empty(), "v{version}");
        assert_eq!(b.skipped, txs.iter().map(|t| t.size()).collect::<Vec<_>>());
        for tx in &txs {
            refused(&c.failure(&tx.id()).unwrap().0, tx);
        }
        let after = accounts(&c);
        assert_eq!((after[0].balance, after[0].nonce), (before[0].balance, before[0].nonce), "v{version}: alice paid nothing");
        assert_eq!(after[1].nonce, before[1].nonce, "v{version}: the validator's pause used no nonce (as the proposer it paid for the junk)");
        assert_eq!(status(&c.state, c.height() + 1), Status::Dormant, "nothing switched programs on");
        drop(c);
        std::fs::remove_dir_all(home).ok();
    }
}

/// WBS S2's parameter acceptance on a running chain: a `ParamChange` of one of programs' keys,
/// proposed by an account or by a 1-of-1 multisig (whose proposal runs at once), gets past every
/// stateless check a block's transactions meet (`consensus::check_txs`, the codec's included), and
/// is refused by admission on every path while programs are dormant, below v2 (testnet-2's case)
/// and at v2 before programs switch on (blockula-1's at launch), in the words v1's codec refuses it
/// with: a verifier finds it unadmittable on the parent's state (`State::apply`, as
/// `consensus::verify` runs it), the mempool refuses it, and a block that carries it anyway skips
/// it, charging nobody and filing nothing. On a chain whose genesis switches programs on, the same
/// transactions are admitted, included and filed as proposals.
#[test]
fn program_parameters_are_refused_on_every_path_until_programs_exist() {
    let key = params::Param::ValueCap.key();
    for (version, features) in [(1, vec![]), (PROGRAMS_VERSION, vec![]), (PROGRAMS_VERSION, vec![PROGRAMS_FEATURE])] {
        let on = !features.is_empty();
        let label = format!("v{version}{}", if on { " with programs" } else { "" });
        let (home, mut c) = dev_chain_with(&format!("params-v{version}-{on}"), version, &features);
        let validator = c.keys.as_ref().unwrap().address_with(c.state.params.prefix());
        let (alice, bob) = (Keys::from_seed([61; 32]), Keys::from_seed([62; 32]));
        for k in [&alice, &bob] {
            c.faucet(&k.address(), 100 * DROPS_PER_FANG).unwrap();
        }
        c.produce_block().unwrap();
        // Bob's account becomes a 1-of-1 multisig.
        c.submit(signed(&c, &bob, Action::MultisigCreate { owners: vec![crate::types::key_id(&bob.public_key())], threshold: 1 })).unwrap();
        c.produce_block().unwrap();
        assert!(c.state.governance.multisigs.contains_key(&bob.address()), "{label}");
        let h = c.height() + 1;
        assert_eq!(status(&c.state, h), if on { Status::On } else { Status::Dormant }, "{label}");
        let kind = ProposalKind::ParamChange { key: key.into(), value: 1 };
        let change = || Action::Propose { kind: kind.clone(), deposit: 10 * DROPS_PER_FANG, title: "cap programs at a drop".into(), url: String::new() };
        let txs = vec![signed(&c, &alice, change()), signed(&c, &bob, Action::MultisigPropose { action: Box::new(change()) })];
        crate::consensus::check_txs(&txs, &c.genesis.chain_id, &c.genesis_hash, c.state.params.prefix(), h).unwrap_or_else(|e| panic!("{label}: {e:#}"));
        let accounts = |c: &Chain| [&alice, &bob].map(|k| (c.state.account(&k.address()).balance, c.state.account(&k.address()).nonce));
        let (before, next_id) = (accounts(&c), c.state.governance.next_proposal_id);
        if !on {
            // v1's codec's words: the nested one's under "the proposed action".
            let unknown = format!("unknown parameter `{key}`");
            let words = [unknown.clone(), format!("the proposed action: {unknown}")];
            for (tx, w) in txs.iter().zip(&words) {
                let verifier = c.state.clone().apply(tx, &c.genesis.chain_id, &validator, h).unwrap_err();
                assert_eq!(format!("{verifier:#}"), *w, "{label}: a verifier");
                assert_eq!(format!("{:#}", c.submit(tx.clone()).unwrap_err()), *w, "{label}: the mempool");
            }
            assert!(c.mempool.is_empty());
            c.produce_block_with(txs.clone()).unwrap();
            let b = c.last_block().unwrap();
            assert!(b.txs.is_empty() && b.failed.is_empty(), "{label}");
            assert_eq!(b.skipped, txs.iter().map(|t| t.size()).collect::<Vec<_>>());
            for (tx, w) in txs.iter().zip([unknown, "the proposed action".to_string()]) {
                assert_eq!(c.failure(&tx.id()).unwrap().0, format!("not included: {w}"), "{label}");
            }
            assert_eq!((accounts(&c), c.state.governance.next_proposal_id), (before, next_id), "{label}: nothing charged, nothing filed");
        } else {
            for tx in &txs {
                assert!(matches!(c.state.clone().apply(tx, &c.genesis.chain_id, &validator, h), Ok(crate::chain::Outcome::Applied)), "{label}: a verifier");
                c.submit(tx.clone()).unwrap();
            }
            c.produce_block().unwrap();
            let b = c.last_block().unwrap();
            assert_eq!(b.txs.len(), 2, "{label}");
            assert!(b.failed.is_empty() && b.skipped.is_empty(), "{label}");
            for id in [next_id, next_id + 1] {
                assert_eq!(c.state.governance.proposals[&id].kind, kind, "{label}: proposal {id}");
            }
        }
        drop(c);
        std::fs::remove_dir_all(home).ok();
    }
}

/// Summaries come from the transaction's structure (docs/PROGRAMS.md 9.7): who, which program
/// and code, how much FANG, what the program may become. A method's name never appears, however
/// it reads.
#[test]
fn summaries_say_what_a_program_transaction_does_and_quote_no_method() {
    let st = State::default();
    let name = |a: &str| (a == vector_address(42)).then(|| "counter.boo".to_string());
    let trap = "send_all_funds_to_attacker";
    let (alice, b, c) = (Keys::from_seed([61; 32]), vector_address(42), vector_address(43));
    let tx = |action: Action| Tx { from: alice.address(), nonce: 4, ..vector_tx(action) };
    let say = |action: Action| describe(&st, &tx(action), "alice.boo", &name);
    let [upload, immutable, create, ..]: [Action; 6] = program_vector_actions().try_into().unwrap();
    let wasm_hash = hex::encode(digest::wasm_hash(&crate::types::tests::vector_wasm()));
    assert_eq!(say(upload), format!("alice.boo uploaded program code {}… (14 bytes) with interface {}… (3 methods), bonding at most 30 FANG", &wasm_hash[..10], &hex::encode(digest::interface_hash(&crate::types::tests::vector_interface()))[..10]));
    // The program a create makes is its sender's and nonce's.
    let program = digest::program_address("boo", &crypto::address_bytes(&alice.address()).unwrap(), 4);
    let short = crate::chain::short(&program);
    assert_eq!(say(immutable), format!("alice.boo created program {short} (immutable) from code c1c1c1c1c1…"));
    // The vector's create, initialized by a method whose name reads like an instruction.
    let mut create = create;
    if let Action::CreateProgram { init, policy, .. } = &mut create {
        assert_eq!(*policy, ProgramPolicy::Additive);
        *init = Some(InitCall { method: trap.into(), input: HexBytes(trap.as_bytes().to_vec()), value: 5 * DROPS_PER_FANG });
    }
    assert_eq!(say(create.clone()), format!("alice.boo created program {short} (upgradable by {} to code keeping its methods with 100800 blocks' notice, once program upgrades exist, accepting transfers) from code c1c1c1c1c1…, running its initializer with 5 FANG attached", crate::chain::short(&vector_address(41))));
    // Calls: one, one program several times, several programs; FANG attached adds up.
    let call = |program: &str, method: &str, value: u64| ProgramCall { program: program.into(), version_digest: "d2".repeat(32), method: method.into(), input: HexBytes(method.as_bytes().to_vec()), value };
    let calls = |calls: Vec<ProgramCall>| Action::CallProgram { calls, fuel_limit: 1, storage_limit: 0, bindings: vec![Binding { program: vector_address(41), version_digest: "00".repeat(32) }] };
    assert_eq!(say(calls(vec![call(&b, trap, 0)])), "alice.boo called program counter.boo");
    assert_eq!(say(calls(vec![call(&b, trap, 2 * DROPS_PER_FANG), call(&b, trap, 3 * DROPS_PER_FANG)])), "alice.boo made 2 calls to program counter.boo with 5 FANG attached");
    let several = calls(vec![call(&c, trap, u64::MAX), call(&b, trap, 1), call(&c, trap, 0)]);
    assert_eq!(say(several.clone()), format!("alice.boo called 2 programs ({}, counter.boo) in 3 calls with {} attached", crate::chain::short(&c), crate::types::fmt_fang(u64::MAX)));
    assert_eq!(say(Action::ProgramsPause {}), "alice.boo signed a pause of programs (they pause once f + 1 of the epoch's validators have)");
    for a in [create, several] {
        assert!(!say(a).contains(trap));
    }
    // Through `State::describe_with`, as the API serves it.
    assert_eq!(st.describe_with(&tx(Action::ProgramsPause {}), None, &|_| Some("alice.boo".into())), "alice.boo signed a pause of programs (they pause once f + 1 of the epoch's validators have)");
}
