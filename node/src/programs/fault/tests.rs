//! Node faults on every path (docs/PROGRAMS-V0.md §6.2, §6.3; WBS S4b's acceptance), injected
//! through `ProgramView`'s test hook: a panic inside the VM (a `Tx` fault) or a thread the OS
//! refuses three times in a row (a `Setup` fault), for one transaction by its id, wherever it runs.
//! A proposal leaves the transaction out and packs the rest; verification abstains in its three
//! places; executing a block halts with 13 or 14 and commits nothing; a restarted node faults
//! again, and its startup replay makes `run_validator` and `run_full_node` exit 13.

use super::super::probe::*;
use super::super::receipt::ReceiptOutcome;
use super::super::view::inject::{self, Injected};
use super::*;
use crate::chain::Outcome;
use crate::consensus::finalized_from;
use crate::store;
use crate::types::Action;
use crate::upgrade::{EXIT_PROGRAM_FAULT, EXIT_PROGRAM_RESOURCES};
use std::time::{Duration, Instant};

/// `what` armed for `tx` until this is dropped, so a failing assertion can't leave it armed.
pub(super) struct Armed(String);

impl Armed {
    pub(super) fn new(tx: &Tx, what: Injected) -> Armed {
        inject::arm(&tx.id(), what);
        Armed(tx.id())
    }
}

impl Drop for Armed {
    fn drop(&mut self) {
        inject::disarm(&self.0);
    }
}

/// The fault counters are this process's, and the tests run as threads of one process, so a count
/// a test reads would also move for another test's faults. Every test whose faults a path counts
/// (the ones that read [`tally`], and the one that only proposes) holds this from just before its
/// first count to its end: what [`moved`] says then is that test's own faults, exactly. (The cells
/// can't be given one per test: seven tests meet faults in a table of eight, and the ancestors
/// test needs a `Propose` cell and a `Verify` one.) `commit_block_refuses_a_state_that_faulted`
/// meets a fault no path counts, and the startup-exit test's faults are in a process of their own.
pub(crate) static COUNTING: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Every cell of the counters: path, class, how many faults it has counted.
pub(super) type Cells = Vec<(Path, FaultClass, u64)>;

pub(super) fn tally() -> Cells {
    Path::ALL.iter().flat_map(|&path| CLASSES.iter().map(move |&class| (path, class, count(path, class)))).collect()
}

/// The cells that counted since `before` ([`tally`]) and how many each: the whole table, so a fault
/// counted under another path or class shows too.
pub(super) fn moved(before: &Cells) -> Cells {
    tally().into_iter().zip(before).filter(|(now, then)| now.2 != then.2).map(|(now, then)| (now.0, now.1, now.2 - then.2)).collect()
}

/// The halt `e` is.
pub(super) fn halt_of(e: &anyhow::Error) -> Halt {
    e.downcast_ref::<Halt>().cloned().unwrap_or_else(|| panic!("not a halt: {e:#}"))
}

/// What the panic the view's hook injects is, as the node records it.
pub(super) const PANIC: &str = "a panic in program execution";

/// Executing a certified block (§6.2's third row): a `Tx` fault halts with `ProgramFault` (exit
/// 13), recorded in the node's `HaltSignal` and returned as the error, and the block is never
/// committed, not even the transactions before it. Restarted, the node executes the block again
/// and faults again; once nothing faults, the same block executes. A thread the OS refuses three
/// times is a `Setup` fault: `ProgramResources`, exit 14.
#[test]
fn executing_a_block_halts_13_on_a_tx_fault_and_14_on_a_setup_fault() {
    let mut d = Dev::new("halts");
    let bob = keys(BOB);
    let p = d.deploy(1).remove(0);
    let (height, root) = (d.c.height(), d.c.state.root());
    let pay = d.tx(&keys(CAROL), Action::Transfer { to: d.address(ALICE), amount: FANG });
    let call = d.tx(&bob, calls(vec![d.call(&p, "ret", vec![], 0)], vec![]));
    let _counting = COUNTING.lock();
    let counted = tally();
    let armed = Armed::new(&call, Injected::Panic);
    let halt = halt_of(&d.run(vec![pay.clone(), call.clone()]).unwrap_err());
    assert_eq!(halt, Halt::ProgramFault { height: height + 1, tx: call.id(), fault: PANIC.into() });
    assert_eq!(halt.exit_code(), EXIT_PROGRAM_FAULT);
    assert_eq!(d.c.halt.signal.get(), Some(halt.clone()), "what node::watch_halts exits with");
    assert_eq!(moved(&counted), [(Path::Execute, FaultClass::Tx, 1)]);
    assert_eq!(d.c.height(), height, "never executed");
    // Restarted: nothing of the block was committed, and executing it again faults again.
    d.reopen().unwrap();
    assert_eq!((d.c.height(), d.c.state.root(), d.c.halt.signal.get()), (height, root, None));
    assert_eq!(halt_of(&d.run(vec![pay.clone(), call.clone()]).unwrap_err()), halt);
    assert_eq!(moved(&counted), [(Path::Execute, FaultClass::Tx, 2)]);
    // Nothing faults any more: the same block executes.
    drop(armed);
    d.reopen().unwrap();
    let b = d.block(vec![pay, call]);
    assert!(b.failed.is_empty() && b.programs[&1].outcome.is_ok(), "{:?}", b.failed);

    // A thread the OS refuses three times: `Setup`, exit 14.
    let call = d.tx(&bob, calls(vec![d.call(&p, "ret", vec![], 0)], vec![]));
    let counted = tally();
    let _armed = Armed::new(&call, Injected::RefuseSpawns(3));
    let halt = halt_of(&d.run(vec![call.clone()]).unwrap_err());
    assert!(matches!(&halt, Halt::ProgramResources { height: h, fault } if *h == d.next() && fault.starts_with("the OS refused a program thread 3 times")), "{halt:?}");
    assert_eq!(halt.exit_code(), EXIT_PROGRAM_RESOURCES);
    assert_eq!(moved(&counted), [(Path::Execute, FaultClass::Setup, 1)]);
}

/// `State::commit_block` refuses a state that hit a node fault, whatever path got it there (§6.2's
/// backstop), with the fault's halt, before anything reaches the tree; the same block on a state
/// that didn't fault commits.
#[test]
fn commit_block_refuses_a_state_that_faulted() {
    let mut d = Dev::new("commit");
    let p = d.deploy(1).remove(0);
    let call = d.tx(&keys(BOB), calls(vec![d.call(&p, "ret", vec![], 0)], vec![]));
    let (h, sink, chain_id, root) = (d.next(), d.c.me().unwrap(), d.c.genesis.chain_id.clone(), d.c.state.root());
    let (mut faulted, mut clean) = (d.c.state.clone(), d.c.state.clone());
    {
        let _armed = Armed::new(&call, Injected::Panic);
        assert!(matches!(faulted.apply(&call, &chain_id, &sink, h), Ok(Outcome::Failed(_))));
    }
    assert_eq!(clean.apply(&call, &chain_id, &sink, h).unwrap(), Outcome::Applied);
    let halt = halt_of(&faulted.commit_block().unwrap_err());
    assert_eq!(halt, Halt::ProgramFault { height: h, tx: call.id(), fault: PANIC.into() });
    assert_eq!(faulted.root(), root, "nothing reached the tree");
    clean.commit_block().unwrap();
    assert_ne!(clean.root(), root);
}

/// A node that faults replaying its block log at startup (`Chain::replay`, §6.2) halts there as
/// executing the block would: `Chain::open`'s error is the `Halt` (exit 13), before the fault
/// could pass for the logged outcome. Once nothing faults, it opens at the same height and root.
#[test]
fn a_startup_replay_that_faults_halts_13_and_opens_once_nothing_faults() {
    let mut d = Dev::new("startup");
    let p = d.deploy(1).remove(0);
    let call = d.tx(&keys(BOB), calls(vec![d.call(&p, "ret", vec![], 0)], vec![]));
    d.block(vec![call.clone()]);
    let at = d.c.height();
    d.block(vec![]);
    let (height, root) = (d.c.height(), d.c.state.root());
    drop(d.take());
    std::fs::remove_file(d.home.join(store::DB_FILE)).unwrap();
    let _counting = COUNTING.lock();
    let counted = tally();
    {
        let _armed = Armed::new(&call, Injected::Panic);
        let halt = halt_of(&d.reopen().unwrap_err());
        assert_eq!(halt, Halt::ProgramFault { height: at, tx: call.id(), fault: PANIC.into() });
        assert_eq!(halt.exit_code(), EXIT_PROGRAM_FAULT);
    }
    assert_eq!(moved(&counted), [(Path::Startup, FaultClass::Tx, 1)]);
    d.reopen().unwrap();
    assert_eq!((d.c.height(), d.c.state.root()), (height, root));
}

/// The child half of [`run_validator_and_run_full_node_exit_13_when_their_startup_replay_faults`],
/// which runs this test binary again for it: `run_validator` or `run_full_node` (`BLOCKULA_S4B_ROLE`)
/// on the home `BLOCKULA_S4B_HOME`, with the transaction `BLOCKULA_S4B_TX` armed to panic. Either
/// exits; returning is a failure (99).
#[test]
#[ignore = "the child process of run_validator_and_run_full_node_exit_13_when_their_startup_replay_faults"]
fn startup_exit_child() {
    let var = |k: &str| std::env::var(k).ok();
    let (Some(home), Some(tx), Some(role)) = (var("BLOCKULA_S4B_HOME"), var("BLOCKULA_S4B_TX"), var("BLOCKULA_S4B_ROLE")) else { return };
    inject::arm(&tx, Injected::Panic);
    let net = crate::node::NetOpts {
        bind: "127.0.0.1".into(),
        http_bind: None,
        p2p_bind: None,
        http_port: None,
        p2p_preset: None,
        ephemeral_p2p: true,
        client_ip: Default::default(),
        peer_http: vec![],
        force_key: false,
        full: false,
        pruning: Default::default(),
        http: Default::default(),
        halt_height: None,
        network_halted_at: None,
    };
    let returned = if role == "validator" { crate::node::run_validator(home.into(), net) } else { crate::node::run_full_node(home.into(), net) };
    eprintln!("{role} returned instead of exiting: {returned:?}");
    std::process::exit(99);
}

/// §6.2's startup exit: a validator (`run_validator`) and a full node (`run_full_node`) whose
/// startup replay faults exit through `exit_halted` with the halt's code, 13, not 1 (each in a
/// child process: they exit).
#[test]
fn run_validator_and_run_full_node_exit_13_when_their_startup_replay_faults() {
    let mut d = Dev::new("exit");
    let p = d.deploy(1).remove(0);
    let call = d.tx(&keys(BOB), calls(vec![d.call(&p, "ret", vec![], 0)], vec![]));
    d.block(vec![call.clone()]);
    drop(d.take());
    std::fs::remove_file(d.home.join(store::DB_FILE)).unwrap();
    std::fs::write(d.home.join("join.json"), r#"{"peers":[]}"#).unwrap();
    for role in ["validator", "full"] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "programs::fault::tests::startup_exit_child", "--ignored", "--nocapture", "--test-threads=1"])
            .env("BLOCKULA_S4B_HOME", &d.home)
            .env("BLOCKULA_S4B_TX", call.id())
            .env("BLOCKULA_S4B_ROLE", role)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // A node that didn't exit would run on: give it a minute, then stop it.
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() > Duration::from_secs(60) {
                child.kill().ok();
                panic!("{role}: still running after a minute");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let mut stderr = String::new();
        std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
        assert_eq!(status.code(), Some(EXIT_PROGRAM_FAULT), "{role}: {stderr}");
        assert!(stderr.contains(&format!("can't be executed here: a program execution fault in tx {}", call.id())) && stderr.contains("exit code 13 (program_fault)"), "{role}: {stderr}");
    }
}

/// A proposal (§6.2's first row, §6.3): Alice's call faults in its trial, so it's left out with
/// her transfer behind it, and both leave this node's mempool, saying why; Bob's call fails in its
/// trial (an abort) and is proposed all the same, and Carol's transfer too. Verifiers accept the
/// block, and executing it charges Bob's call, with its receipt.
#[test]
fn a_proposal_leaves_out_a_tx_whose_trial_faults_drops_it_and_packs_the_rest() {
    let mut d = Dev::new("propose");
    let (alice, bob, carol) = (keys(ALICE), keys(BOB), keys(CAROL));
    let p = d.deploy(1).remove(0);
    let faulting = d.txs(&alice, vec![calls(vec![d.call(&p, "ret", vec![], 0)], vec![]), Action::Transfer { to: d.address(CAROL), amount: FANG }]);
    let failing = d.tx(&bob, calls(vec![d.call(&p, "no", vec![], 0)], vec![]));
    let pay = d.tx(&carol, Action::Transfer { to: d.address(BOB), amount: FANG });
    for t in faulting.iter().chain([&failing, &pay]) {
        d.c.submit(t.clone()).unwrap();
    }
    let _counting = COUNTING.lock();
    let counted = tally();
    let armed = Armed::new(&faulting[0], Injected::Panic);
    let r = Round2::new(&mut d, &[]);
    let block = r.propose().expect("a proposal");
    let has = |t: &Tx| block.txs.contains(&t.to_bytes());
    assert!(has(&failing) && has(&pay), "the rest is packed");
    assert!(!has(&faulting[0]) && !has(&faulting[1]), "the faulted tx is left out, and its sender's next");
    assert_eq!(block.txs.len(), 2);
    assert_eq!(moved(&counted), [(Path::Propose, FaultClass::Tx, 1)]);
    {
        let c = r.chain.lock();
        assert!(!c.in_mempool(&faulting[0].id()) && !c.in_mempool(&faulting[1].id()));
        assert_eq!(c.failure(&faulting[0].id()).unwrap().0, format!("not proposed: program execution faulted on this node ({PANIC})"));
        assert_eq!(c.failure(&faulting[1].id()).unwrap().0, "dropped from the mempool: an earlier transaction from the same account was dropped");
        assert!(c.in_mempool(&failing.id()) && c.in_mempool(&pay.id()), "the rest wait for their block");
    }
    drop(armed);
    assert_eq!(r.verify(block.clone()), Some(true));
    {
        let mut c = r.chain.lock();
        c.execute(finalized_from(&r.parent, None)).unwrap();
        let bob_at = |c: &Chain| c.state.account(&failing.from).balance;
        let (before, charged) = (bob_at(&c), crate::econ::check_fee(&c.state, &failing, block.height).unwrap());
        c.execute(finalized_from(&block, None)).unwrap();
        let b = c.last_block().unwrap().clone();
        let at = b.txs.iter().position(|t| t.id() == failing.id()).unwrap();
        assert_eq!(b.failed.keys().copied().collect::<Vec<_>>(), vec![at], "only Bob's call fails");
        assert_eq!(b.programs[&at].outcome, ReceiptOutcome::Aborted { program: p.clone(), abort_code: 7, untrusted_message: "e".into() });
        assert_eq!(b.failed[&at], format!("program {} aborted with code 7", crate::chain::short(&p)));
        // Charged: its fee, as its receipt's parts say (Carol's transfer paid Bob 1 FANG).
        let fee = &b.programs[&at].fee;
        assert_eq!((bob_at(&c), fee.bytes + fee.fuel + fee.tip), (before - charged + FANG, charged));
    }
    r.give_back(&mut d);
}

/// A faulted trial's rollback gives back what the trial took, the fuel it reserved among it (§6.3):
/// otherwise a tx that faults on this node would crowd honest program txs out of its proposal. The
/// block's program fuel is 20,000,000 (the lowest the parameter allows), so one call of 10,000,000
/// fuel (and its load) fits and two don't. Alice's tipped call is tried first and faults; Bob's
/// call, the same size, must still be packed.
#[test]
fn a_faulted_trial_gives_back_the_fuel_it_reserved() {
    use super::super::params::{Param, MIN_BLOCK_FUEL};
    let mut d = Dev::new("fuel-back");
    let (alice, bob) = (keys(ALICE), keys(BOB));
    let p = d.deploy(1).remove(0);
    d.c.state.programs.meta.as_mut().unwrap().params.insert(Param::MaxBlockFuel.key().into(), MIN_BLOCK_FUEL);
    let big = |d: &Dev| Action::CallProgram { calls: vec![d.call(&p, "ret", vec![], 0)], fuel_limit: 10_000_000, storage_limit: FANG, bindings: vec![] };
    let size = d.tx(&alice, big(&d)).size();
    let tip = crate::econ::base_fee(&d.c.state, d.next() + 1, size);
    let faulting = d.txs_at(&alice, None, tip, vec![big(&d)]).remove(0);
    let ok = d.tx(&bob, big(&d));
    for t in [&faulting, &ok] {
        d.c.submit(t.clone()).unwrap();
    }
    let r = Round2::new(&mut d, &[]);
    let _counting = COUNTING.lock();
    let counted = tally();
    let armed = Armed::new(&faulting, Injected::Panic);
    let block = r.propose().expect("a proposal");
    assert_eq!(block.txs, vec![ok.to_bytes()], "the faulted trial's fuel went back with its rollback");
    assert_eq!(moved(&counted), [(Path::Propose, FaultClass::Tx, 1)]);
    drop(armed);
    r.give_back(&mut d);
}

/// A `Setup` fault in a proposal's trial (a thread the OS refused three times) skips the
/// proposal's other program transactions, and drops nothing from the mempool: the fault is this
/// node's, not theirs. A spawn refused twice, then allowed, is no fault at all.
#[test]
fn a_setup_fault_holds_the_proposals_other_program_txs_back_and_drops_nothing() {
    let mut d = Dev::new("setup");
    let (alice, bob, carol) = (keys(ALICE), keys(BOB), keys(CAROL));
    let p = d.deploy(1).remove(0);
    // Alice tips, so her call is tried first.
    let probe = d.tx(&alice, calls(vec![d.call(&p, "ret", vec![], 0)], vec![]));
    let tip = crate::econ::base_fee(&d.c.state, d.next() + 1, probe.size());
    let first = d.txs_at(&alice, None, tip, vec![calls(vec![d.call(&p, "ret", vec![], 0)], vec![])]).remove(0);
    let second = d.tx(&bob, calls(vec![d.call(&p, "ret", vec![], 0)], vec![]));
    let pay = d.tx(&carol, Action::Transfer { to: d.address(BOB), amount: FANG });
    for t in [&first, &second, &pay] {
        d.c.submit(t.clone()).unwrap();
    }
    let _counting = COUNTING.lock();
    let counted = tally();
    let r = Round2::new(&mut d, &[]);
    {
        let _armed = Armed::new(&first, Injected::RefuseSpawns(3));
        let block = r.propose().expect("a proposal");
        assert_eq!(block.txs, vec![pay.to_bytes()], "no program tx after a Setup fault");
        assert_eq!(moved(&counted), [(Path::Propose, FaultClass::Setup, 1)]);
        let c = r.chain.lock();
        assert!([&first, &second, &pay].iter().all(|t| c.in_mempool(&t.id()) && c.failure(&t.id()).is_none()), "nothing dropped");
    }
    {
        let _armed = Armed::new(&first, Injected::RefuseSpawns(2));
        let block = r.propose().expect("a proposal");
        assert_eq!(block.txs.len(), 3, "two refusals, then a thread: no fault");
        assert_eq!(block.txs[0], first.to_bytes());
        assert_eq!(moved(&counted), [(Path::Propose, FaultClass::Setup, 1)], "and no more counted");
    }
    r.give_back(&mut d);
}

/// A fault replaying the parent's unexecuted ancestors (§6.2): this node can't judge the parent,
/// so it proposes nothing on it, and abstains on a child of it. Once nothing faults it proposes,
/// and votes for, the child.
#[test]
fn a_parent_whose_replay_faults_gets_no_proposal_and_its_child_no_vote() {
    let mut d = Dev::new("ancestors");
    let p = d.deploy(1).remove(0);
    let call = d.tx(&keys(ALICE), calls(vec![d.call(&p, "ret", vec![], 0)], vec![]));
    let r = Round2::new(&mut d, std::slice::from_ref(&call));
    {
        let _counting = COUNTING.lock();
        let _armed = Armed::new(&call, Injected::Panic);
        let counted = tally();
        assert!(r.propose().is_none(), "no proposal on a parent this node can't replay");
        assert_eq!(moved(&counted), [(Path::Propose, FaultClass::Tx, 1)]);
        assert_eq!(r.verify(r.block(&[])), None, "no vote on its child");
        assert_eq!(moved(&counted), [(Path::Propose, FaultClass::Tx, 1), (Path::Verify, FaultClass::Tx, 1)]);
    }
    assert!(r.propose().is_some());
    assert_eq!(r.verify(r.block(&[])), Some(true));
    r.give_back(&mut d);
}

/// Verification (§6.2's second row) abstains on a fault in the block's own transactions: after its
/// loop, and on an unadmittable transaction while a fault is set (an earlier one's fault may be
/// why), where it would otherwise reject. Without the fault, it accepts the first block and rejects
/// the second.
#[test]
fn verification_abstains_on_a_fault_in_the_blocks_own_txs() {
    let mut d = Dev::new("verify");
    let p = d.deploy(1).remove(0);
    let call = d.tx(&keys(ALICE), calls(vec![d.call(&p, "ret", vec![], 0)], vec![]));
    // Carol's nonce is 0: a transfer at 5 can't be admitted, though it passes every stateless check.
    let gap = d.txs_at(&keys(CAROL), Some(5), 0, vec![Action::Transfer { to: d.address(BOB), amount: FANG }]).remove(0);
    let r = Round2::new(&mut d, &[]);
    let (lone, unadmittable) = (r.block(std::slice::from_ref(&call)), r.block(&[call.clone(), gap]));
    {
        let _counting = COUNTING.lock();
        let _armed = Armed::new(&call, Injected::Panic);
        let counted = tally();
        assert_eq!(r.verify(lone.clone()), None, "a fault in its txs: abstain after the loop");
        assert_eq!(moved(&counted), [(Path::Verify, FaultClass::Tx, 1)]);
        assert_eq!(r.verify(unadmittable.clone()), None, "an unadmittable tx while a fault is set: abstain, not reject");
        assert_eq!(moved(&counted), [(Path::Verify, FaultClass::Tx, 2)]);
    }
    assert_eq!(r.verify(lone), Some(true));
    assert_eq!(r.verify(unadmittable), Some(false));
    r.give_back(&mut d);
}

/// `/metrics` counts node faults by path and class (`blockula_program_node_faults_total`), with the
/// other program lines, once programs exist.
#[test]
fn node_faults_are_counted_by_path_and_class_in_metrics() {
    let text = metrics();
    assert!(text.starts_with("# HELP blockula_program_node_faults_total ") && text.contains("# TYPE blockula_program_node_faults_total counter\n"));
    for path in ["propose", "verify", "execute", "startup"] {
        for class in ["tx", "setup"] {
            assert!(text.contains(&format!("blockula_program_node_faults_total{{path=\"{path}\",class=\"{class}\"}} ")), "{path} {class}: {text}");
        }
    }
    let d = Dev::new("metrics");
    assert!(super::super::api::metrics(&d.c.view()).contains("blockula_program_node_faults_total{path=\"execute\",class=\"tx\"} "));
}
