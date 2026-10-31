//! The proposer's rule for program transactions that fail (docs/PROGRAMS-V0.md §6.3; WBS S3's arm
//! in `consensus.rs`'s `propose`), through `App::propose` itself: `Chain::produce_block` executes
//! the mempool directly and never runs the proposer. A program transaction that is admitted and
//! then fails in the proposer's trial ran (or was set to run) at the proposer's expense, so it is
//! proposed with its fee phase kept: charged, its nonce used. Any other transaction that would fail
//! is left out, as before programs, and so are its sender's later ones.

use crate::consensus::{finalized_from, hex_32, App, CBlock, Shared, SUPPORTED_PROTOCOL_VERSION};
use crate::pq::PrivateKey;
use crate::programs::fixtures::*;
use crate::programs::{status, Status};
use crate::types::{Action, Tx, WASM_HEADER};
use commonware_consensus::simplex::types::Context as SimplexContext;
use commonware_consensus::types::{Epoch, Round, View};
use commonware_consensus::{marshal, Application};
use commonware_cryptography::{blake3::Digest, Digestible as _, Signer as _};
use commonware_runtime::{deterministic, Runner as _, Supervisor as _};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

/// What every transaction here pays at most (only the charge is debited).
const FEE: u64 = FANG / 100;

/// On a dev chain with programs on, four transactions wait in the mempool, each admitted on the
/// state it saw: Alice's upload of code that fails validation (it passes every check before
/// execution, whose validation refuses the module), then Alice's transfer at the next nonce;
/// Carol's release of her name, tipping so the proposal takes it first; and Dave's payment to that
/// name. The proposer's trial applies the release, then finds Dave's payment failing (the name is
/// gone) and leaves it out, and finds Alice's upload failing and includes it, keeping its fee
/// phase: her transfer behind it applies only because the upload's nonce stayed used in the trial.
/// Verifiers accept the block, and its execution charges the upload as failed.
#[test]
fn a_failed_program_tx_is_proposed_and_any_other_failed_tx_is_left_out() {
    let (alice, carol, dave) = (keys(61), keys(63), keys(64));
    let (home, mut c) = dev_chain("propose", &[]);
    for k in [&alice, &carol, &dave] {
        c.faucet(&k.address(), 100 * FANG).unwrap();
    }
    c.produce_block().unwrap();
    c.submit(signed(&c, &carol, 0, FEE, Action::RegisterName { name: "nightjar".into() })).unwrap();
    c.produce_block().unwrap();
    let e = c.height();
    let me = c.me().unwrap();

    let mut wasm = WASM_HEADER.to_vec();
    wasm.extend([0xff; 8]);
    let failing = signed(&c, &alice, 0, FEE, upload(&wasm, COUNTER.1, 0));
    let behind = signed(&c, &alice, 1, FEE, Action::Transfer { to: me.clone(), amount: FANG });
    let release = signed(&c, &carol, 1, FEE, Action::ReleaseName { name: "nightjar".into() });
    let release = Tx { tip: crate::econ::base_fee(&c.state, e + 1, release.size()), ..release }.sign(&carol);
    let pay = signed(&c, &dave, 0, FEE, Action::Transfer { to: "nightjar.boo".into(), amount: FANG });
    for t in [&failing, &behind, &release, &pay] {
        c.submit(t.clone()).unwrap();
    }
    assert!(crate::programs::is_program_tx(&failing) && !crate::programs::is_program_tx(&pay));

    // The proposal is for e + 2, on an empty block e + 1 that is ordered but not executed yet (the
    // proposer replays it), so the fee slot is known and the parent's timestamp is the simulated
    // clock's start.
    assert_eq!(status(&c.state, e + 2), Status::On);
    let version = crate::upgrade::version_at(&c.state, e + 1);
    let parent = CBlock::new(Digest(hex_32(c.last_hash())), e + 1, e + 1, e, 0, String::new(), e, hex_32(&c.root_at(e).unwrap()), vec![]).with_versions(version, SUPPORTED_PROTOCOL_VERSION);
    let key_seed: [u8; 32] = hex::decode(std::fs::read_to_string(home.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
    let prefix = c.state.params.prefix().to_string();
    let chain: Shared = Arc::new(Mutex::new(c));
    let block = deterministic::Runner::timed(Duration::from_secs(60)).start(|context| {
        let (chain, parent) = (chain.clone(), parent.clone());
        async move {
            let leader = PrivateKey::from_seed_bytes(key_seed).public_key();
            let mut app = App { chain, block_time_ms: 1_000, address_prefix: prefix, replay_cache: Default::default() };
            let cctx = SimplexContext { round: Round::new(Epoch::zero(), View::new(e + 2)), leader, parent: (View::new(parent.view), parent.digest()) };
            let ancestry = marshal::ancestry::from_iter([Arc::new(parent.clone())]);
            let block = Application::<deterministic::Context>::propose(&mut app, (context.child("propose"), cctx.clone()), ancestry, ()).await.expect("proposes");
            let ancestry = marshal::ancestry::from_iter([Arc::new(block.clone()), Arc::new(parent)]);
            assert!(Application::<deterministic::Context>::verify(&mut app, (context.child("verify"), cctx), ancestry).await, "verifiers accept a failed program tx");
            block
        }
    });
    let has = |t: &Tx| block.txs.contains(&t.to_bytes());
    assert!(has(&release), "the release applies");
    assert!(has(&failing), "the failed upload is proposed");
    assert!(has(&behind), "Alice's next nonce applies: the failed upload's nonce stayed used");
    assert!(!has(&pay), "a failed transfer is left out");
    assert_eq!(block.txs.len(), 3);

    let mut c = chain.lock();
    c.execute(finalized_from(&parent, None)).unwrap();
    c.execute(finalized_from(&block, None)).unwrap();
    let b = c.last_block().unwrap();
    let at = b.txs.iter().position(|t| t.id() == failing.id()).unwrap();
    assert_eq!(b.failed.keys().copied().collect::<Vec<_>>(), vec![at], "only the upload fails, and it's charged: {:?}", b.failed);
    assert!(b.failed[&at].starts_with("invalid code: upload validation rule "), "{}", b.failed[&at]);
    assert_eq!(c.state.account(&alice.address()).nonce, 2);
    // Dave's payment would have failed: once the block ran, the mempool drops it saying why.
    assert!(!c.in_mempool(&pay.id()));
    let why = &c.failure(&pay.id()).unwrap().0;
    assert!(why.contains("no one has claimed nightjar.boo yet"), "{why}");
    drop(c);
    std::fs::remove_dir_all(home).ok();
}
