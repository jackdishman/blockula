//! Admission end to end (WBS T's acceptance, docs/PROGRAMS-V0.md §2.4, §5.4): on S4b's probe chain (a
//! dev chain born at v2 with programs on, one validator), a call that names a wrong version digest
//! is refused at admission on every path a transaction takes to a block, and never charged. S4a's
//! `a_wrong_digest_fails_in_the_vm` shows the VM's own check, with admission's refusal read
//! directly; this runs the whole chain: the mempool, the block's verification and its execution
//! (`produce_block_with`, which skips what admission refuses).

use super::super::pass::{counts, Counts};
use super::super::probe::*;
use crate::chain::short;
use crate::types::ProgramCall;

/// A call of the probe's `ret` naming a version digest that isn't the one in force is refused at
/// admission in the words of §2.4's check: by the mempool when it's submitted (never queued), by
/// verification (a block carrying it is rejected, while the same call at the right digest is
/// accepted), and by execution (the transaction is skipped: its sender is charged nothing, its
/// nonce stays, no engine starts, no fuel is counted and the rate doesn't move; the proposer pays
/// the skipped transaction's bytes' base fee, burned, and nothing for fuel). The same call at
/// the right digest then runs and pays its fuel at the rate in force.
#[test]
fn a_call_with_a_wrong_digest_is_refused_at_admission_and_never_charged() {
    let mut d = Dev::new("wrong-digest");
    let alice = keys(ALICE);
    let p = d.deploy(1).remove(0);
    let right = d.call(&p, "ret", vec![], 0);
    let in_force = right.version_digest.clone();
    let wrong = ProgramCall { version_digest: "11".repeat(32), ..right.clone() };
    let refusal = format!("call 1 names version 1111111111… of program {}, whose version in force is {}…", short(&p), &in_force[..10]);
    let bad = d.tx(&alice, calls(vec![wrong], vec![]));
    let good = d.tx(&alice, calls(vec![right], vec![]));
    assert_eq!((bad.nonce, bad.size()), (good.nonce, good.size()), "the two differ in the digest alone");

    // The mempool refuses it, and queues nothing.
    let e = d.c.submit(bad.clone()).unwrap_err().to_string();
    assert!(e.contains(&refusal), "{e}");
    assert!(!d.c.in_mempool(&bad.id()));

    // Verification rejects a block that carries it (a verdict, not an abstention), and accepts the
    // same call at the right digest.
    let r = Round2::new(&mut d, &[]);
    assert_eq!(r.verify(r.block(std::slice::from_ref(&good))), Some(true));
    assert_eq!(r.verify(r.block(std::slice::from_ref(&bad))), Some(false));
    r.give_back(&mut d);

    // Execution skips it: nothing charged, nothing counted, nothing run.
    let (balance, nonce) = (d.balance(ALICE), d.c.state.account(&bad.from).nonce);
    let (started, rate) = (counts(), d.c.state.programs.meta.as_ref().unwrap().fuel_rate);
    let height = d.next();
    // What execution does charge for a transaction it skips: the proposer pays its bytes' base fee as
    // junk (§5.4: never fuel, since it never ran), and the fee is burned.
    let me = d.c.me().unwrap();
    let junk = crate::econ::base_fee(&d.c.state, height, bad.size());
    let (burned, supply, proposer) = (d.c.state.burned, d.c.state.supply, d.c.state.account(&me).balance);
    d.c.produce_block_with(vec![bad.clone()]).unwrap();
    let b = d.c.last_block().unwrap().clone();
    assert_eq!(b.height, height);
    assert!(b.txs.is_empty() && b.failed.is_empty() && b.programs.is_empty(), "the block holds no transaction of hers");
    let why = d.c.failure(&bad.id()).expect("it says why").0.clone();
    assert!(why.starts_with("not included: ") && why.contains(&refusal), "{why}");
    assert_eq!((d.balance(ALICE), d.c.state.account(&bad.from).nonce), (balance, nonce), "never charged, its nonce unused");
    assert_eq!(d.c.state.programs.block.fuel_at(height), 0, "no fuel counted");
    assert_eq!(d.c.state.programs.meta.as_ref().unwrap().fuel_rate, rate, "an empty block at the floor leaves the rate there");
    assert_eq!(counts().since(started), Counts { pass_threads: 1, tx_threads: 0, engines: 0, upload_checks: 0 }, "no program ran: the block carries a program transaction, so its pass has a thread, and the skipped one ran nothing on it");
    let started = counts();
    assert_eq!((d.c.state.burned - burned, supply - d.c.state.supply, proposer - d.c.state.account(&me).balance), (junk, junk, junk), "the proposer's junk charge, bytes only, burned");

    // The same call at the right digest, at the same nonce: included, run, and charged what §5.1
    // says, written out here: its bytes' base fee, and at the floor rate (250 milli-drops per fuel)
    // its `fuel_limit` and the load of the probe's code (10,000 and 40 a byte) of fuel. The burn is
    // that charge, out of her balance.
    let height = d.next();
    let fuel = 250 * (FUEL + 10_000 + 40 * PROBE_WASM.len() as u64) / 1_000;
    let charge = crate::econ::base_fee(&d.c.state, height, good.size()) + fuel;
    let burned = d.c.state.burned;
    d.c.produce_block_with(vec![good.clone()]).unwrap();
    let b = d.c.last_block().unwrap().clone();
    assert_eq!((b.txs.len(), b.failed.len()), (1, 0));
    assert_eq!(counts().since(started), Counts { pass_threads: 1, tx_threads: 0, engines: 1, upload_checks: 0 });
    assert_eq!((balance - d.balance(ALICE), d.c.state.burned - burned), (charge, charge));
    assert_eq!(d.c.state.account(&good.from).nonce, nonce + 1);
}
